// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Source guards for the shared core (ADR 0067 §2).
//!
//! 1. **Purity.** `apprafter-core/src` never prints, ends the process,
//!    takes a terminal handle, reads or writes the environment, or asks
//!    `dirs` for a platform directory — neither through `std`, nor through
//!    `libc` or the Win32 environment API (the core links both for
//!    `process::run_bounded`), nor through a lower-crate function that does
//!    it on the core's behalf ([`FORBIDDEN_CALLEES`]).
//! 2. **Dependencies.** `apprafter-core` depends on no prompt, progress,
//!    table, colour or signal crate ([`FORBIDDEN_DEPS`]).
//! 3. **Ratchet.** Env reads in the crates the core builds on, and in the
//!    CLI, may only go down: each slice that moves a family into the core
//!    moves its env reads behind `Context`. The count is exact, so a drop
//!    must lower [`ENV_READ_BASELINE`] in the same commit and can never
//!    creep back up.
//! 4. **No TypeScript export.** Nothing in `apprafter-core/src` or
//!    `apprafter-core/tests`, test code included, asks ts-rs to write a
//!    binding — neither a `ts(export)` attribute, with any delimiter, nor a
//!    direct `TS::export` / `TS::export_all` call ([`ts_exports`]): under
//!    cli/'s `cargo test --all-features` it would write files into this
//!    crate. The desktop exports the core's types from its own tests.
//! 5. **One door to the secrets.** No production code in `apprafter-core/src` outside
//!    `src/secrets/` reads or writes a target's credentials file, reads the shared age key, or
//!    opens or seals a cache ([`SECRET_DOORS`], [`SECRET_DOOR_METHODS`]), and nothing in
//!    `apprafter-core/src` seals a cache to a recipient that is not a `CacheKey`'s ([`SEAL`]):
//!    every token and cached secret goes through `Context::secrets()` (ADR 0068 §5; D.4a
//!    T16), and the same holds in `platform-cli/src`, where only the seal itself is not a door
//!    (its recipient must be a `CacheKey`'s); the shared key is created only by the store
//!    (`SecretStore::new_cache`, in files).
//!    The vault's own doors ([`VAULT_DOORS`]) are named only in `src/secrets/`, `src/lock/` and
//!    `src/vault/`, and never in `platform-cli` or the desktop; the lock flows' file doors
//!    ([`TRANSFER_DOOR`]) only in `src/secrets/` and `src/lock/`.
//! 6. **Test-only key derivation stays in tests (L4).** No manifest under `cli/` or
//!    `desktop/` turns on `apprafter-core/insecure-fast-kdf` outside `[dev-dependencies]`,
//!    except the desktop's `test-build` ([`INSECURE_KDF_FORWARDERS`]), and none of the
//!    core's own features pulls it in (D.4a T23).
//! 7. **The vault's code rules.** In `src/vault/`'s production code no `==` / `!=` touches
//!    key material ([`VAULT_SECRET_NAMES`]; `subtle::ConstantTimeEq` only, at least
//!    [`VAULT_CT_EQ_MIN`] times), no argon2 call allocates its own working memory
//!    ([`UNZEROED_ARGON2`]), and no type derives `Serialize` or `Deserialize` (D.4a T17, T29).
//! 8. **L6.** `platform-cli` and the desktop never name a lock flow's fault or its test KDF
//!    policy ([`TEST_ONLY_LOCK_KNOBS`]).
//! 9. **One writer of `state.json` (WI-487).** No production code in `apprafter-core/src` or
//!    `platform-cli/src` saves a `State` itself ([`STATE_SAVE`], a `.save(…)` call): every
//!    write goes through `cli_state::State::update`, which reads the file again under the
//!    store's write lock, so no writer saves a copy it read before the lock.
//!
//! The scan reads syntax trees (`syn`), not text, so neither a comment nor
//! a string can hide or fake a hit, and nothing depends on where an item
//! sits in its file:
//!
//! - an item, impl item, statement, match arm or field whose `cfg` implies
//!   `test` (`cfg(test)`, `cfg(all(test, …))`) is skipped with all it
//!   contains, and so is the file of an out-of-line `#[cfg(test)] mod x;`
//!   (`x.rs`, and everything under `x/`); everything else is scanned;
//! - a path resolves through the `use` declarations in scope, renames and
//!   globs included, so `use std::env::var as v; v("X")` is
//!   `std::env::var`;
//! - macro arguments are scanned as expressions when they parse as such,
//!   else token by token, so `format!("{}", std::env::var("X")?)` counts;
//! - `env!` / `option_env!` read the build environment at compile time and
//!   are not runtime reads.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ImplItem, Item, Meta, Stmt, TraitItem, UseTree};

/// Production env reads ([`Kind::EnvRead`] and [`Kind::Dirs`]) in
/// [`RATCHETED_CRATES`]. Re-measure, with a per-crate and per-callee
/// breakdown, with `cargo test -p apprafter-core --test guards -- --nocapture`. 50 since
/// d4a-e: `cli_core::paths::darwin_user_temp_dir`, the holder's last macOS base (T49, W5); 49
/// since d4a-d: `cli_core::paths::local_data_dir`, the CLI's Windows runtime dir (T44).
const ENV_READ_BASELINE: usize = 50;

/// The crates the core builds on, and the CLI.
const RATCHETED_CRATES: &[&str] = &["cli-core", "cli-state", "cli-providers", "platform-cli"];

/// Functions that read the process environment: `std`'s, and `libc`'s.
/// `home_dir` reads `HOME` (`USERPROFILE` on Windows), un-deprecated in Rust
/// 1.87, so `-D warnings` no longer stops it.
const ENV_READS: &[&str] = &[
    "std::env::var",
    "std::env::var_os",
    "std::env::vars",
    "std::env::vars_os",
    "std::env::current_dir",
    "std::env::temp_dir",
    "std::env::home_dir",
    "libc::getenv",
    "libc::secure_getenv",
    // A per-user lookup that ignores the environment (macOS `_CS_DARWIN_USER_TEMP_DIR`): the
    // holder's base reaches it only through one sanctioned cli-core wrapper (D.4a T8, G2).
    "libc::confstr",
];

/// Modules whose every item reads or writes the process environment,
/// counted as [`Kind::EnvRead`]: the Win32 environment-block API
/// (`GetEnvironmentVariableW`, `SetEnvironmentVariableW`,
/// `GetEnvironmentStringsW`, `SetCurrentDirectoryW`, …).
const ENV_MODULES: &[&str] = &["windows_sys::Win32::System::Environment"];

/// Functions that change the process environment (the current directory
/// included: [`ENV_READS`] counts reading it).
const ENV_WRITES: &[&str] = &[
    "std::env::set_var",
    "std::env::remove_var",
    "std::env::set_current_dir",
    "libc::setenv",
    "libc::unsetenv",
    "libc::putenv",
    "libc::clearenv",
];

/// `std` functions that end the process.
const PROCESS_ENDS: &[&str] = &["std::process::exit", "std::process::abort"];

/// `std` functions that hand out the process's terminal streams.
const TERMINAL: &[&str] = &["std::io::stdout", "std::io::stderr", "std::io::stdin"];

/// Macros that write to stdout or stderr, by their last path segment.
const PRINT_MACROS: &[&str] = &["print", "println", "eprint", "eprintln", "dbg"];

/// Lower-crate items that read the environment or touch the terminal on
/// their caller's behalf; a path matches an entry when it starts with it, so
/// `KubectlCli::default()` and a `KubectlCli` type both count. Paths resolve
/// as written, so each re-export is listed beside its definition. Explicit
/// `_from(..)` variants are added to the lower crates as each consumer needs
/// one, and this list grows with the crates (D.3a added the key-writing,
/// `PATH`- and `CUE_BIN`-reading, SSH-identity and legacy-state entries);
/// every entry must name a real item ([`every_listed_callee_names_a_real_item`]).
const FORBIDDEN_CALLEES: &[&str] = &[
    // `APPRAFTER_SSH_PUBLIC_KEY`, then the target store.
    "cli_core::credentials::resolve_hetzner_ssh_public_key",
    "cli_core::resolve_hetzner_ssh_public_key",
    // `APPRAFTER_CONFIG_DIR`, then `dirs`.
    "cli_core::target::default_config_root",
    "cli_core::default_config_root",
    // `RUST_LOG`, and installs a stderr subscriber.
    "cli_core::logging::init",
    // Run kubectl / helm children with inherited stdio.
    "cli_providers::k8s::kubectl::KubectlCli",
    "cli_providers::k8s::KubectlCli",
    "cli_providers::k8s::helm::HelmCli",
    "cli_providers::k8s::HelmCli",
    // Writes a new key when none exists: a read never writes; key creation is D.4's.
    "cli_core::secrets::load_or_create_identity",
    // Read `PATH`: the core resolves tools on `Context::tool_search_path`.
    "cli_core::tools::preflight_tool",
    "cli_core::tools::preflight_tools",
    // Read `CUE_BIN` and the current directory.
    "cli_core::cue::export",
    "cli_core::cue::export_in",
    // `APPRAFTER_SSH_PRIVATE_KEY`, then `dirs`.
    "cli_providers::hetzner_cloud::kubeconfig::default_ssh_identity_path",
    "cli_providers::hetzner_cloud::default_ssh_identity_path",
    // Prints; the legacy `<cwd>/.apprafter` migration is CLI-only (spec §3.1).
    "cli_state::migrate_legacy_state_if_present",
    "cli_state::state::migrate_legacy_state_if_present",
    // `APPRAFTER_SKIP_NODE_SWAP`.
    "cli_providers::hetzner_cloud::user_data::swap_eligible_from_env",
    "cli_providers::hetzner_cloud::swap_eligible_from_env",
];

/// Callees the core may reach from the functions paired with them, and from
/// nowhere else; a callee may be paired with several sites, one entry each.
/// `config_root_from_override` takes the override value explicitly, but
/// without one it consults the platform config directory through `dirs`
/// (HOME / XDG on Unix, the Known Folder API on Windows). Both clients'
/// context builders resolve the store root through it — the CLI in
/// `Context::from_cli_env`, the desktop in `Context::from_desktop_env` — so
/// both open the same default target store. `dirs::*` inside
/// `apprafter-core/src` itself stays forbidden.
const SANCTIONED: &[(&str, &str)] = &[
    (
        "cli_core::target::config_root_from_override",
        "Context::from_cli_env",
    ),
    (
        "cli_core::config_root_from_override",
        "Context::from_cli_env",
    ),
    (
        "cli_core::target::config_root_from_override",
        "Context::from_desktop_env",
    ),
    (
        "cli_core::config_root_from_override",
        "Context::from_desktop_env",
    ),
    // `dirs::home_dir`: the age key's default and the `~/` of displayed paths.
    ("cli_core::paths::home_dir", "Context::from_cli_env"),
    ("cli_core::paths::home_dir", "Context::from_desktop_env"),
    // `dirs::data_local_dir`: the CLI's runtime dir on Windows (D.4a T44).
    ("cli_core::paths::local_data_dir", "Context::from_cli_env"),
    // `confstr(_CS_DARWIN_USER_TEMP_DIR)`: macOS's per-user temporary directory, the holder's
    // last base (D.4a T49, W5), for the CLI's context only.
    (
        "cli_core::paths::darwin_user_temp_dir",
        "Context::from_cli_env",
    ),
];

/// What reads or writes a target's `credentials.yaml`, reads the shared age key, or opens or
/// seals a cached secret. In `apprafter-core/src` only `src/secrets/` may reach them: every
/// other module goes through `Context::secrets()` (ADR 0068 §5; D.4a T16). A path matches an
/// entry when it starts with it; paths resolve as written, so each re-export is listed beside
/// its definition, and every entry must name a real item
/// (`every_listed_callee_names_a_real_item`).
const SECRET_DOORS: &[&str] = &[
    "cli_core::target::load_target",
    "cli_core::load_target",
    "cli_core::target::load_target_credentials",
    "cli_core::target::read_target_credentials_bytes",
    "cli_core::target::parse_target_credentials",
    "cli_core::target::save_target",
    "cli_core::save_target",
    "cli_core::target::save_target_credentials",
    "cli_core::secrets::load_identity",
    // Read the shared age key, and create it when it is missing: a key file the lock of a
    // locked store would not cover (D.4a c2).
    "cli_core::secrets::load_or_create_identity",
    "cli_core::secrets::load_or_create_identity_reporting",
    "cli_core::secrets::decrypt_with_identity",
    "cli_core::secrets::decrypt_with_identity_reason",
    "cli_core::secrets::encrypt_for_recipient",
];

/// Methods that name a credentials file. A method call is matched by its name: a syntax scan
/// does not know the receiver's type, and no other method is called this.
const SECRET_DOOR_METHODS: &[&str] = &["target_credentials_file"];

/// The one function a cache is sealed with, and the one method its recipient may come from,
/// in the same expression: `CacheKey::recipient` (D.4a T16).
const SEAL: &str = "cli_core::secrets::encrypt_for_recipient";
const RECIPIENT_METHOD: &str = "recipient";

/// The vault's own doors (c1's hand-off; D.4a T16): its file reads and writes, and the one type
/// that parses and opens a vault. In `apprafter-core/src` only `secrets/`, `lock/` and `vault/`
/// itself may name them; `platform-cli` and the desktop never do — they open a store through
/// `lock::open_at` and `Context::secrets()`.
const VAULT_DOORS: &[&str] = &[
    "crate::vault::file",
    "crate::vault::VaultFile",
    "apprafter_core::vault::file",
    "apprafter_core::vault::VaultFile",
];

/// What the lock flows move between a store's files and its vault (D.4a T16; critic finding
/// 9): `secrets::transfer` reads credentials files, writes clear tokens and seals caches, so
/// only the secret store and the lock flows name it. It is `pub(crate)`: outside the core it
/// cannot be named at all.
const TRANSFER_DOOR: &str = "crate::secrets::transfer";

/// L6 (D.4a critic G12): what only an in-process core test may name — the fault a lock flow
/// is interrupted at, and the weak KDF policy it runs under. The CLI and the desktop never do,
/// in code or tests, so no build of them arms a fault or weakens a policy.
const TEST_ONLY_LOCK_KNOBS: &[&str] = &["FaultPoint", "with_fault", "with_kdf_policy"];

/// WI-487: `state.json`'s raw save, for tests and `State::update` alone. Called as a path, or
/// as a method named `save` — the only `save` method in the scanned crates is this one.
const STATE_SAVE: &str = "cli_state::State::save";
const STATE_SAVE_METHOD: &str = "save";

/// Crates `apprafter-core` must not depend on: a client prompts, draws
/// progress, renders tables and colours, and handles signals.
///
/// Only `[dependencies]` and `[target.*.dependencies]` are checked:
/// `[dev-dependencies]` and `[build-dependencies]` never reach the shipped
/// binary, so they may use any of these.
const FORBIDDEN_DEPS: &[&str] = &[
    "inquire",
    "indicatif",
    "tabled",
    "dialoguer",
    "console",
    "signal-hook",
    "ctrlc",
    "owo-colors",
];

/// G2 (D.4a T8): the holder's endpoint is derived in one function from `Context` inputs, never
/// from a uid (`/run/user/<uid>`) or a fixed pipe prefix (`\\.\pipe\`). The functions (`fn` or
/// `Type::fn`, as [`SANCTIONED`] names sites) allowed to spell one, in the ratcheted crates and
/// the core: d4a-e's `pipe_name` and `ssh_agent_path`.
const ENDPOINT_LITERALS: &[&str] = &[
    // The Windows holder's pipe (D.4a T50): `platform-cli/src/holder/endpoint.rs`.
    "pipe_name",
    // The Windows OpenSSH agent's pipe (D.4a T72, O1): `platform-cli/src/limits_client.rs`.
    "ssh_agent_path",
];

/// A string literal G2 keeps in [`ENDPOINT_LITERALS`].
fn is_endpoint_literal(value: &str) -> bool {
    value.contains("/run/user") || value.contains(r"\\.\pipe\")
}

// ---------------------------------------------------------------------
// The scanner
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// One of [`ENV_READS`], or anything under one of [`ENV_MODULES`].
    EnvRead,
    /// Any `dirs::*` function: it reads HOME / XDG (or asks Windows).
    Dirs,
    /// One of [`ENV_WRITES`].
    EnvWrite,
    /// One of [`PROCESS_ENDS`].
    ProcessEnd,
    /// One of [`TERMINAL`].
    Terminal,
    /// One of [`PRINT_MACROS`].
    Print,
    /// One of [`FORBIDDEN_CALLEES`], or a [`SANCTIONED`] callee off its site.
    ForbiddenCallee,
    /// A `ts(export)` / `ts(export_to = …)` attribute, or a direct
    /// `TS::export*` call ([`ts_exports`]).
    TsExport,
    /// One of [`SECRET_DOORS`] or [`SECRET_DOOR_METHODS`]: allowed only under
    /// `apprafter-core/src/secrets/`.
    SecretDoor,
    /// One of [`VAULT_DOORS`]: allowed only under `apprafter-core/src/{secrets,lock,vault}/`.
    VaultDoor,
    /// [`TRANSFER_DOOR`]: allowed only under `apprafter-core/src/{secrets,lock}/`.
    TransferDoor,
    /// A call of [`SEAL`] whose recipient is not `<key>.recipient()` (a `CacheKey`'s), anywhere.
    ForeignRecipient,
    /// A per-user runtime path or a Windows pipe name outside [`ENDPOINT_LITERALS`] (G2).
    EndpointLiteral,
    /// [`STATE_SAVE`], or a `.save(…)` method call: `state.json` saved outside
    /// `State::update` (WI-487).
    StateSave,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Finding {
    file: PathBuf,
    line: usize,
    kind: Kind,
    /// The resolved path, or the list entry it matched.
    callee: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = self.file.strip_prefix(cli_dir()).unwrap_or(&self.file);
        write!(
            f,
            "{}:{}: {:?} `{}`",
            file.display(),
            self.line,
            self.kind,
            self.callee
        )
    }
}

#[derive(Debug, Default)]
struct Scan {
    findings: Vec<Finding>,
    /// Files of out-of-line modules gated by `cfg(test)`.
    test_module_files: Vec<PathBuf>,
    /// Directories whose every file belongs to such a module.
    test_module_dirs: Vec<PathBuf>,
}

/// The names one scope (a file, module, block, function, closure, arm or
/// `if let` / `while let` / `for`) brings in.
#[derive(Debug, Default)]
struct Scope {
    /// `alias` → the full path it stands for.
    aliases: HashMap<String, Vec<String>>,
    /// Paths imported with `::*`.
    globs: Vec<Vec<String>>,
    /// Bindings visited so far (`let`, parameters, patterns). A binding
    /// shadows an import of the same name, in its own scope too.
    locals: HashSet<String>,
}

impl Scope {
    fn of_items<'i>(items: impl IntoIterator<Item = &'i Item>) -> Self {
        let mut scope = Scope::default();
        for item in items {
            if let Item::Use(u) = item {
                if !cfg_implies_test(&u.attrs) {
                    collect_use(&u.tree, &mut Vec::new(), &mut scope);
                }
            }
        }
        scope
    }
}

fn collect_use(tree: &UseTree, prefix: &mut Vec<String>, scope: &mut Scope) {
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            collect_use(&p.tree, prefix, scope);
            prefix.pop();
        }
        UseTree::Name(n) if n.ident == "self" => {
            if let Some(last) = prefix.last() {
                scope.aliases.insert(last.clone(), prefix.clone());
            }
        }
        UseTree::Name(n) => {
            let mut full = prefix.clone();
            full.push(n.ident.to_string());
            scope.aliases.insert(n.ident.to_string(), full);
        }
        UseTree::Rename(r) if r.rename == "_" => {}
        UseTree::Rename(r) => {
            let mut full = prefix.clone();
            if r.ident != "self" {
                full.push(r.ident.to_string());
            }
            scope.aliases.insert(r.rename.to_string(), full);
        }
        UseTree::Glob(_) => scope.globs.push(prefix.clone()),
        UseTree::Group(g) => {
            for t in &g.items {
                collect_use(t, prefix, scope);
            }
        }
    }
}

/// Whether a `cfg` predicate holds only in test builds.
fn implies_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(p) => p.is_ident("test"),
        Meta::List(list) => {
            let Ok(args) =
                list.parse_args_with(Punctuated::<Meta, syn::Token![,]>::parse_terminated)
            else {
                return false;
            };
            if list.path.is_ident("all") {
                args.iter().any(implies_test)
            } else if list.path.is_ident("any") {
                !args.is_empty() && args.iter().all(implies_test)
            } else {
                false
            }
        }
        Meta::NameValue(_) => false,
    }
}

fn cfg_implies_test(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("cfg") && a.parse_args::<Meta>().is_ok_and(|m| implies_test(&m)))
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn impl_item_attrs(item: &ImplItem) -> &[Attribute] {
    match item {
        ImplItem::Const(i) => &i.attrs,
        ImplItem::Fn(i) => &i.attrs,
        ImplItem::Type(i) => &i.attrs,
        ImplItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &TraitItem) -> &[Attribute] {
    match item {
        TraitItem::Const(i) => &i.attrs,
        TraitItem::Fn(i) => &i.attrs,
        TraitItem::Type(i) => &i.attrs,
        TraitItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

/// The attributes of an expression, for every kind that carries them (syn
/// 2): a `#[cfg(test)]` on a `loop`, `for` or `return` statement gates it as
/// surely as one on a call. Only `Verbatim` has none; `Expr` is
/// non-exhaustive, so a kind a newer syn adds falls there too.
fn expr_attrs(e: &Expr) -> &[Attribute] {
    match e {
        Expr::Array(x) => &x.attrs,
        Expr::Assign(x) => &x.attrs,
        Expr::Async(x) => &x.attrs,
        Expr::Await(x) => &x.attrs,
        Expr::Binary(x) => &x.attrs,
        Expr::Block(x) => &x.attrs,
        Expr::Break(x) => &x.attrs,
        Expr::Call(x) => &x.attrs,
        Expr::Cast(x) => &x.attrs,
        Expr::Closure(x) => &x.attrs,
        Expr::Const(x) => &x.attrs,
        Expr::Continue(x) => &x.attrs,
        Expr::Field(x) => &x.attrs,
        Expr::ForLoop(x) => &x.attrs,
        Expr::Group(x) => &x.attrs,
        Expr::If(x) => &x.attrs,
        Expr::Index(x) => &x.attrs,
        Expr::Infer(x) => &x.attrs,
        Expr::Let(x) => &x.attrs,
        Expr::Lit(x) => &x.attrs,
        Expr::Loop(x) => &x.attrs,
        Expr::Macro(x) => &x.attrs,
        Expr::Match(x) => &x.attrs,
        Expr::MethodCall(x) => &x.attrs,
        Expr::Paren(x) => &x.attrs,
        Expr::Path(x) => &x.attrs,
        Expr::Range(x) => &x.attrs,
        Expr::RawAddr(x) => &x.attrs,
        Expr::Reference(x) => &x.attrs,
        Expr::Repeat(x) => &x.attrs,
        Expr::Return(x) => &x.attrs,
        Expr::Struct(x) => &x.attrs,
        Expr::Try(x) => &x.attrs,
        Expr::TryBlock(x) => &x.attrs,
        Expr::Tuple(x) => &x.attrs,
        Expr::Unary(x) => &x.attrs,
        Expr::Unsafe(x) => &x.attrs,
        Expr::While(x) => &x.attrs,
        Expr::Yield(x) => &x.attrs,
        _ => &[],
    }
}

/// The expression a statement's attributes are attached to. syn hangs
/// them on the leftmost operand, not on the expression the statement is:
/// `#[cfg(test)] x = y;` parses as an `Assign` whose `left` carries the
/// `cfg`, and so do a `Binary`'s `left` and a `Cast`'s `expr`, at any
/// depth (`#[cfg(test)] a + b as u8 == c;` gates `a`).
fn leftmost_operand(e: &Expr) -> &Expr {
    match e {
        Expr::Assign(x) => leftmost_operand(&x.left),
        Expr::Binary(x) => leftmost_operand(&x.left),
        Expr::Cast(x) => leftmost_operand(&x.expr),
        _ => e,
    }
}

/// The directory a file's out-of-line `mod x;` declarations live in.
fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or_else(|| Path::new(""));
    match file.file_name().and_then(|n| n.to_str()) {
        Some("lib.rs" | "main.rs" | "mod.rs") => parent.to_path_buf(),
        _ => parent.join(file.file_stem().expect("a .rs file has a stem")),
    }
}

fn line_of(span: Span) -> usize {
    span.start().line
}

fn path_matches(path: &[String], entry: &str) -> bool {
    let entry: Vec<&str> = entry.split("::").collect();
    path.len() >= entry.len() && path.iter().zip(&entry).all(|(a, b)| a == b)
}

/// Whether `e` is a recipient a `CacheKey` gave — `<key>.recipient()`, behind any `&`, `?` and
/// parentheses. One bound to a name first does not count: the rule is that the recipient is
/// made in the very expression handed to [`SEAL`], where the guard can read it.
fn from_cache_key(e: &Expr) -> bool {
    match e {
        Expr::Reference(r) => from_cache_key(&r.expr),
        Expr::Try(t) => from_cache_key(&t.expr),
        Expr::Paren(p) => from_cache_key(&p.expr),
        Expr::MethodCall(m) => m.method == RECIPIENT_METHOD && m.args.is_empty(),
        _ => false,
    }
}

struct Scanner<'f> {
    file: &'f Path,
    scopes: Vec<Scope>,
    /// Inline `mod a { … }` names around the current item.
    inline_mods: Vec<String>,
    /// Self types of the enclosing `impl` blocks, innermost last.
    impl_types: Vec<String>,
    /// `Type::name` / `name` of the enclosing functions, innermost last.
    fns: Vec<String>,
    /// Inside an attribute, a doc comment included: a literal there spells no endpoint.
    in_attribute: bool,
    scan: Scan,
}

impl<'f> Scanner<'f> {
    /// Every full path `segs` may stand for. Scopes are searched innermost
    /// first, as in Rust: a local binding of a one-segment value path stands
    /// for no path at all (a `macro_name` lives in its own namespace and
    /// skips that rule), and an explicit import of the first segment wins;
    /// failing both, the path as written and the path under each glob in
    /// scope.
    fn candidates(&self, segs: &[String], macro_name: bool) -> Vec<Vec<String>> {
        for scope in self.scopes.iter().rev() {
            if !macro_name && segs.len() == 1 && scope.locals.contains(&segs[0]) {
                return Vec::new();
            }
            if let Some(full) = scope.aliases.get(&segs[0]) {
                let mut p = full.clone();
                p.extend_from_slice(&segs[1..]);
                return vec![p];
            }
        }
        let mut out = vec![segs.to_vec()];
        for scope in &self.scopes {
            for g in &scope.globs {
                let mut p = g.clone();
                p.extend_from_slice(segs);
                out.push(p);
            }
        }
        out
    }

    fn classify(&self, path: &[String]) -> Option<(Kind, String)> {
        let joined = path.join("::");
        let lists: [(&[&str], Kind); 4] = [
            (ENV_READS, Kind::EnvRead),
            (ENV_WRITES, Kind::EnvWrite),
            (PROCESS_ENDS, Kind::ProcessEnd),
            (TERMINAL, Kind::Terminal),
        ];
        for (list, kind) in lists {
            if list.contains(&joined.as_str()) {
                return Some((kind, joined));
            }
        }
        if let Some(module) = ENV_MODULES
            .iter()
            .find(|m| path_matches(path, m) && path.len() > m.split("::").count())
        {
            return Some((Kind::EnvRead, module.to_string()));
        }
        if path.len() >= 2 && path[0] == "dirs" {
            return Some((Kind::Dirs, joined));
        }
        if let Some(entry) = FORBIDDEN_CALLEES.iter().find(|e| path_matches(path, e)) {
            return Some((Kind::ForbiddenCallee, entry.to_string()));
        }
        if let Some(entry) = SECRET_DOORS.iter().find(|e| path_matches(path, e)) {
            return Some((Kind::SecretDoor, entry.to_string()));
        }
        if let Some(entry) = VAULT_DOORS.iter().find(|e| path_matches(path, e)) {
            return Some((Kind::VaultDoor, entry.to_string()));
        }
        if path_matches(path, TRANSFER_DOOR) {
            return Some((Kind::TransferDoor, TRANSFER_DOOR.to_string()));
        }
        if joined == STATE_SAVE || joined == "cli_state::state::State::save" {
            return Some((Kind::StateSave, STATE_SAVE.to_string()));
        }
        // A sanctioned callee is allowed when ANY entry pairs it with the
        // current site, and forbidden everywhere else.
        let site = self.fns.last().map(String::as_str);
        let mut pairs = SANCTIONED.iter().filter(|(e, _)| path_matches(path, e));
        let (entry, _) = pairs.clone().next()?;
        if pairs.any(|(_, allowed)| site == Some(*allowed)) {
            return None;
        }
        Some((Kind::ForbiddenCallee, entry.to_string()))
    }

    fn check_path(&mut self, segs: &[String], span: Span) {
        let hit = self
            .candidates(segs, false)
            .iter()
            .find_map(|p| self.classify(p));
        if let Some((kind, callee)) = hit {
            self.record(kind, callee, span);
        }
    }

    fn check_macro_name(&mut self, segs: &[String], span: Span) {
        let is_print = self.candidates(segs, true).iter().any(|p| {
            p.last()
                .is_some_and(|last| PRINT_MACROS.contains(&last.as_str()))
        });
        if is_print {
            let name = segs.last().expect("a macro path has a name");
            self.record(Kind::Print, format!("{name}!"), span);
        }
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(Scope::default());
        f(self);
        self.scopes.pop();
    }

    fn record(&mut self, kind: Kind, callee: String, span: Span) {
        self.scan.findings.push(Finding {
            file: self.file.to_path_buf(),
            line: line_of(span),
            kind,
            callee,
        });
    }

    /// G2: record `value` when it spells an endpoint outside [`ENDPOINT_LITERALS`].
    fn check_literal(&mut self, value: &str, span: Span) {
        if self.in_attribute || !is_endpoint_literal(value) {
            return;
        }
        let site = self.fns.last().cloned().unwrap_or_default();
        if ENDPOINT_LITERALS.contains(&site.as_str()) {
            return;
        }
        self.record(
            Kind::EndpointLiteral,
            format!("{value:?} in `{site}`"),
            span,
        );
    }

    fn skip_out_of_line(&mut self, m: &syn::ItemMod) {
        let explicit = m.attrs.iter().find_map(|a| match &a.meta {
            Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
                Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        });
        let mut dir = module_dir(self.file);
        if let Some(rel) = explicit {
            if self.inline_mods.is_empty() {
                let base = self.file.parent().unwrap_or_else(|| Path::new(""));
                self.scan.test_module_files.push(base.join(rel));
                return;
            }
        }
        dir.extend(&self.inline_mods);
        let name = m.ident.to_string();
        self.scan
            .test_module_files
            .push(dir.join(format!("{name}.rs")));
        self.scan.test_module_dirs.push(dir.join(name));
    }

    /// A macro body: as comma-separated expressions, else as statements,
    /// else token by token.
    fn scan_macro_body(&mut self, mac: &syn::Macro) {
        if let Ok(args) = mac.parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated)
        {
            for e in &args {
                self.visit_expr(e);
            }
        } else if let Ok(stmts) = mac.parse_body_with(syn::Block::parse_within) {
            self.scopes
                .push(Scope::of_items(stmts.iter().filter_map(|s| match s {
                    Stmt::Item(i) => Some(i),
                    _ => None,
                })));
            for s in &stmts {
                self.visit_stmt(s);
            }
            self.scopes.pop();
        } else {
            self.scan_tokens(mac.tokens.clone());
        }
    }

    /// The fallback for a body that does not parse: every `a::b::c` run of
    /// identifiers is a path, and `name!` a macro, except after a `.` (a
    /// method or field, not a path).
    fn scan_tokens(&mut self, tokens: TokenStream) {
        let tts: Vec<TokenTree> = tokens.into_iter().collect();
        let is_punct =
            |i: usize, c: char| matches!(tts.get(i), Some(TokenTree::Punct(p)) if p.as_char() == c);
        let mut i = 0;
        while i < tts.len() {
            match &tts[i] {
                TokenTree::Group(g) => {
                    self.scan_tokens(g.stream());
                    i += 1;
                }
                TokenTree::Ident(id) => {
                    let after_dot = i > 0 && is_punct(i - 1, '.');
                    let span = id.span();
                    let mut segs = vec![id.to_string()];
                    let mut j = i + 1;
                    while is_punct(j, ':') && is_punct(j + 1, ':') {
                        match tts.get(j + 2) {
                            Some(TokenTree::Ident(next)) => {
                                segs.push(next.to_string());
                                j += 3;
                            }
                            _ => break,
                        }
                    }
                    if !after_dot {
                        if is_punct(j, '!') && matches!(tts.get(j + 1), Some(TokenTree::Group(_))) {
                            self.check_macro_name(&segs, span);
                        } else {
                            self.check_path(&segs, span);
                        }
                    }
                    i = j;
                }
                TokenTree::Literal(lit) => {
                    if let Ok(s) = syn::parse_str::<syn::LitStr>(&lit.to_string()) {
                        self.check_literal(&s.value(), lit.span());
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
    }
}

impl<'ast, 'f> Visit<'ast> for Scanner<'f> {
    fn visit_item(&mut self, item: &'ast Item) {
        if cfg_implies_test(item_attrs(item)) {
            if let Item::Mod(m) = item {
                if m.content.is_none() {
                    self.skip_out_of_line(m);
                }
            }
            return;
        }
        visit::visit_item(self, item);
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        // An out-of-line module's file is scanned on its own.
        if let Some((_, items)) = &m.content {
            self.inline_mods.push(m.ident.to_string());
            self.scopes.push(Scope::of_items(items));
            for item in items {
                self.visit_item(item);
            }
            self.scopes.pop();
            self.inline_mods.pop();
        }
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let name = match &*i.self_ty {
            syn::Type::Path(tp) => tp
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.impl_types.push(name);
        visit::visit_item_impl(self, i);
        self.impl_types.pop();
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        if !cfg_implies_test(impl_item_attrs(item)) {
            visit::visit_impl_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let owner = self.impl_types.last().cloned().unwrap_or_default();
        self.fns.push(format!("{owner}::{}", f.sig.ident));
        self.scoped(|s| visit::visit_impl_item_fn(s, f));
        self.fns.pop();
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.fns.push(f.sig.ident.to_string());
        self.scoped(|s| visit::visit_item_fn(s, f));
        self.fns.pop();
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.scoped(|s| visit::visit_trait_item_fn(s, f));
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        self.scoped(|s| visit::visit_expr_closure(s, c));
    }

    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        self.scoped(|s| visit::visit_expr_if(s, e));
    }

    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.scoped(|s| visit::visit_expr_while(s, e));
    }

    fn visit_expr_for_loop(&mut self, e: &'ast syn::ExprForLoop) {
        self.scoped(|s| visit::visit_expr_for_loop(s, e));
    }

    /// The initialiser runs before the pattern binds: `let v = v("X");`
    /// calls the imported `v`.
    fn visit_local(&mut self, l: &'ast syn::Local) {
        if let Some(init) = &l.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        self.visit_pat(&l.pat);
    }

    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(p.ident.to_string());
        }
        visit::visit_pat_ident(self, p);
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        if !cfg_implies_test(trait_item_attrs(item)) {
            visit::visit_trait_item(self, item);
        }
    }

    fn visit_block(&mut self, b: &'ast syn::Block) {
        self.scopes
            .push(Scope::of_items(b.stmts.iter().filter_map(|s| match s {
                Stmt::Item(i) => Some(i),
                _ => None,
            })));
        visit::visit_block(self, b);
        self.scopes.pop();
    }

    fn visit_stmt(&mut self, s: &'ast Stmt) {
        let gated = match s {
            Stmt::Local(l) => cfg_implies_test(&l.attrs),
            Stmt::Macro(m) => cfg_implies_test(&m.attrs),
            Stmt::Expr(e, _) => {
                cfg_implies_test(expr_attrs(e)) || cfg_implies_test(expr_attrs(leftmost_operand(e)))
            }
            Stmt::Item(_) => false,
        };
        if !gated {
            visit::visit_stmt(self, s);
        }
    }

    fn visit_arm(&mut self, a: &'ast syn::Arm) {
        if !cfg_implies_test(&a.attrs) {
            self.scoped(|s| visit::visit_arm(s, a));
        }
    }

    fn visit_field_value(&mut self, f: &'ast syn::FieldValue) {
        if !cfg_implies_test(&f.attrs) {
            visit::visit_field_value(self, f);
        }
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        let segs: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
        if let Some(first) = p.segments.first() {
            self.check_path(&segs, first.ident.span());
        }
        visit::visit_path(self, p);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let name = m.method.to_string();
        if SECRET_DOOR_METHODS.contains(&name.as_str()) {
            self.record(Kind::SecretDoor, name, m.method.span());
        } else if name == STATE_SAVE_METHOD {
            self.record(Kind::StateSave, format!(".{name}(…)"), m.method.span());
        }
        visit::visit_expr_method_call(self, m);
    }

    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let Expr::Path(p) = &*c.func {
            let segs: Vec<String> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let seals = self
                .candidates(&segs, false)
                .iter()
                .any(|cand| cand.join("::") == SEAL);
            if seals && !c.args.iter().nth(1).is_some_and(from_cache_key) {
                if let Some(first) = p.path.segments.first() {
                    self.record(Kind::ForeignRecipient, SEAL.to_string(), first.ident.span());
                }
            }
        }
        visit::visit_expr_call(self, c);
    }

    fn visit_attribute(&mut self, a: &'ast Attribute) {
        let outer = std::mem::replace(&mut self.in_attribute, true);
        visit::visit_attribute(self, a);
        self.in_attribute = outer;
    }

    fn visit_lit_str(&mut self, l: &'ast syn::LitStr) {
        self.check_literal(&l.value(), l.span());
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let segs: Vec<String> = mac
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        if let Some(last) = mac.path.segments.last() {
            self.check_macro_name(&segs, last.ident.span());
        }
        self.scan_macro_body(mac);
    }
}

/// Scan one source file. `path` names it in findings and anchors its
/// out-of-line modules.
fn scan_source(path: &Path, src: &str) -> Scan {
    let file = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    let mut s = Scanner {
        file: path,
        scopes: Vec::new(),
        inline_mods: Vec::new(),
        impl_types: Vec::new(),
        fns: Vec::new(),
        in_attribute: false,
        scan: Scan::default(),
    };
    if cfg_implies_test(&file.attrs) {
        // `#![cfg(test)]`: the whole module, and its children, are tests.
        s.scan.test_module_files.push(path.to_path_buf());
        s.scan.test_module_dirs.push(module_dir(path));
        return s.scan;
    }
    s.scopes.push(Scope::of_items(&file.items));
    for item in &file.items {
        s.visit_item(item);
    }
    s.scan
}

/// Scan every `.rs` file under `src`, dropping the files of test modules.
fn scan_tree(src: &Path) -> Vec<Finding> {
    let files = rust_files(src);
    assert!(!files.is_empty(), "no Rust sources under {}", src.display());
    let mut findings = Vec::new();
    let mut test_files = Vec::new();
    let mut test_dirs = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        let scan = scan_source(&file, &text);
        findings.extend(scan.findings);
        test_files.extend(scan.test_module_files);
        test_dirs.extend(scan.test_module_dirs);
    }
    findings.retain(|f| {
        !test_files.contains(&f.file) && !test_dirs.iter().any(|d| f.file.starts_with(d))
    });
    findings.sort();
    findings
}

/// The ts-rs functions that write a binding to disk: `TS::export` and
/// `TS::export_all`, and `export_all_to` of earlier ts-rs releases.
const TS_EXPORT_FNS: &[&str] = &["export", "export_all", "export_all_to"];

/// Every place in `src` that asks ts-rs to write a binding, as (line, what
/// matched):
///
/// - an attribute (`#[…]` / `#![…]`) carrying a `ts` followed by a group
///   that names the ident `export` or `export_to`, at any nesting and with
///   any delimiter ([`ts_export_in`]): `#[ts(export)]`, `#[ts{export}]`,
///   `#[ts(rename = "x", export)]`,
///   `#[cfg_attr(feature = "ts", ts(export_to = "x.ts"))]`.
///   `#[ts(rename = "export")]` names a string, not the ident, and passes;
/// - a `::` followed by one of [`TS_EXPORT_FNS`], a direct call
///   (`UiError::export_all(&cfg)`, `<T as TS>::export(&cfg)`). The core
///   has no item of those names, so any such path is ts-rs's; an item that
///   wants one must take another name, as telling the two apart would take
///   type resolution. `x.export()` (a method) and `fn export_report()` pass.
///
/// The file is read as tokens, not as items: a test module and a macro's
/// input are covered too, because an exported binding writes its file
/// whenever the test that ts-rs generates for it runs. A comment never
/// reaches the tokens and a string (a doc comment's text included) is one
/// literal, so neither fakes a hit — this file's own cases sit in strings
/// and never flag it.
fn ts_exports(src: &str) -> Vec<(usize, String)> {
    let tokens: TokenStream = src.parse().unwrap_or_else(|e| panic!("tokenize: {e:?}"));
    let mut hits = Vec::new();
    collect_ts_exports(tokens, &mut hits);
    hits
}

fn collect_ts_exports(tokens: TokenStream, hits: &mut Vec<(usize, String)>) {
    let tts: Vec<TokenTree> = tokens.into_iter().collect();
    let is_punct =
        |i: usize, c: char| matches!(tts.get(i), Some(TokenTree::Punct(p)) if p.as_char() == c);
    for (i, tt) in tts.iter().enumerate() {
        match tt {
            TokenTree::Group(g) => {
                let attribute = g.delimiter() == Delimiter::Bracket
                    && i > 0
                    && (is_punct(i - 1, '#')
                        || (i > 1 && is_punct(i - 1, '!') && is_punct(i - 2, '#')));
                if attribute {
                    if let Some(line) = ts_export_in(g.stream()) {
                        hits.push((line, "ts(export)".into()));
                    }
                }
                collect_ts_exports(g.stream(), hits);
            }
            TokenTree::Ident(id)
                if i > 1
                    && is_punct(i - 2, ':')
                    && is_punct(i - 1, ':')
                    && TS_EXPORT_FNS.contains(&id.to_string().as_str()) =>
            {
                hits.push((line_of(id.span()), format!("::{id}")));
            }
            _ => {}
        }
    }
}

/// The line of the first `ts` in an attribute's tokens that is followed by
/// a group naming `export` or `export_to`, whatever the group's delimiter:
/// syn's `MetaList`, which ts-rs parses its attributes with, takes `( … )`,
/// `[ … ]` and `{ … }` alike, and ts-rs honours all three.
fn ts_export_in(tokens: TokenStream) -> Option<usize> {
    let tts: Vec<TokenTree> = tokens.into_iter().collect();
    tts.iter().enumerate().find_map(|(i, tt)| match tt {
        TokenTree::Ident(id) if id == "ts" => match tts.get(i + 1) {
            Some(TokenTree::Group(g)) if names_export(g.stream()) => Some(line_of(id.span())),
            _ => None,
        },
        TokenTree::Group(g) => ts_export_in(g.stream()),
        _ => None,
    })
}

fn names_export(tokens: TokenStream) -> bool {
    tokens.into_iter().any(|tt| match tt {
        TokenTree::Ident(id) => id == "export" || id == "export_to",
        TokenTree::Group(g) => names_export(g.stream()),
        _ => false,
    })
}

/// [`ts_exports`] over every `.rs` file under the crate's `src` and
/// `tests`, test modules included: an integration test is compiled and run
/// by the same `cargo test --all-features`, so an export there writes into
/// the crate as surely as one in `src`. This file is scanned with the rest.
fn ts_export_crate(krate: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let files = ["src", "tests"]
        .into_iter()
        .flat_map(|dir| rust_files(&krate.join(dir)));
    for file in files {
        let text = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        for (line, callee) in ts_exports(&text) {
            findings.push(Finding {
                file: file.clone(),
                line,
                kind: Kind::TsExport,
                callee,
            });
        }
    }
    findings
}

/// The dependencies of a member manifest that [`FORBIDDEN_DEPS`] names,
/// by their real package name: a `package = "…"` rename counts, in the
/// member or in the workspace entry a `workspace = true` points at, and so
/// does a `[target.'cfg(…)'.dependencies]` table.
fn forbidden_deps(member: &str, workspace: &str) -> Vec<String> {
    let member: toml::Value = member.parse().expect("member Cargo.toml parses");
    let workspace: toml::Value = workspace.parse().expect("workspace Cargo.toml parses");
    let ws_deps = workspace
        .get("workspace")
        .and_then(|w| w.get("dependencies"));
    let mut tables = vec![member.get("dependencies")];
    if let Some(targets) = member.get("target").and_then(|t| t.as_table()) {
        tables.extend(targets.values().map(|t| t.get("dependencies")));
    }
    let mut hits = Vec::new();
    for table in tables.into_iter().flatten().filter_map(|t| t.as_table()) {
        for (key, spec) in table {
            let inherited = spec.get("workspace").and_then(|w| w.as_bool()) == Some(true);
            let source = if inherited {
                ws_deps.and_then(|d| d.get(key)).unwrap_or(spec)
            } else {
                spec
            };
            let name = source
                .get("package")
                .and_then(|p| p.as_str())
                .unwrap_or(key)
                .replace('_', "-");
            if FORBIDDEN_DEPS.contains(&name.as_str()) {
                hits.push(name);
            }
        }
    }
    hits
}

/// Whether `path` (`crate::module::…::name`) names a function, a struct,
/// or a `pub use` of that name in the lower crate's sources.
fn item_exists(path: &str) -> bool {
    let segs: Vec<&str> = path.split("::").collect();
    let (name, modules) = segs.split_last().expect("a non-empty path");
    let mut file = crate_dir(&modules[0].replace('_', "-"))
        .join("src")
        .join("lib.rs");
    for m in &modules[1..] {
        let dir = module_dir(&file);
        let flat = dir.join(format!("{m}.rs"));
        let nested = dir.join(m).join("mod.rs");
        file = if flat.exists() {
            flat
        } else if nested.exists() {
            nested
        } else {
            return false;
        };
    }
    let text = fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
    let parsed = syn::parse_file(&text).unwrap_or_else(|e| panic!("parse {file:?}: {e}"));
    parsed.items.iter().any(|item| match item {
        Item::Fn(f) => f.sig.ident == name,
        Item::Struct(s) => s.ident == name,
        Item::Use(u) if matches!(u.vis, syn::Visibility::Public(_)) => {
            let mut scope = Scope::default();
            collect_use(&u.tree, &mut Vec::new(), &mut scope);
            scope.aliases.contains_key(*name)
        }
        _ => false,
    })
}

fn cli_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("apprafter-core sits in the cli workspace")
        .to_path_buf()
}

fn crate_dir(name: &str) -> PathBuf {
    cli_dir().join(name)
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap_or_else(|e| panic!("read {d:?}: {e}")) {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------
// The guards
// ---------------------------------------------------------------------

#[test]
fn the_core_never_prints_exits_or_reads_the_environment() {
    let core = crate_dir("apprafter-core");
    let mut hits: Vec<Finding> = scan_tree(&core.join("src"))
        .into_iter()
        .filter(|h| {
            !matches!(
                h.kind,
                Kind::SecretDoor
                    | Kind::ForeignRecipient
                    | Kind::VaultDoor
                    | Kind::TransferDoor
                    | Kind::StateSave
            )
        })
        .collect();
    hits.extend(ts_export_crate(&core));
    hits.sort();
    assert!(
        hits.is_empty(),
        "apprafter-core must stay pure (ADR 0067 §2) and export no TypeScript:\n{}",
        hits.iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_core_depends_on_no_prompt_progress_colour_or_signal_crate() {
    let member = fs::read_to_string(crate_dir("apprafter-core").join("Cargo.toml")).unwrap();
    let workspace = fs::read_to_string(cli_dir().join("Cargo.toml")).unwrap();
    let hits = forbidden_deps(&member, &workspace);
    assert!(
        hits.is_empty(),
        "apprafter-core must not depend on {hits:?}: the client prompts, draws and handles signals"
    );
}

#[test]
fn every_listed_callee_names_a_real_item() {
    let stale: Vec<&str> = FORBIDDEN_CALLEES
        .iter()
        .copied()
        .chain(SANCTIONED.iter().map(|(c, _)| *c))
        .chain(SECRET_DOORS.iter().copied())
        .filter(|p| !item_exists(p))
        .collect();
    assert!(
        stale.is_empty(),
        "these entries name nothing — a rename left the guard blind: {stale:?}"
    );
}

#[test]
fn env_reads_outside_the_core_only_go_down() {
    let mut sites: Vec<(&str, Finding)> = Vec::new();
    for krate in RATCHETED_CRATES {
        for f in scan_tree(&crate_dir(krate).join("src")) {
            if matches!(f.kind, Kind::EnvRead | Kind::Dirs) {
                sites.push((krate, f));
            }
        }
    }
    let mut per_crate: BTreeMap<&str, usize> = RATCHETED_CRATES.iter().map(|k| (*k, 0)).collect();
    let mut per_callee: BTreeMap<&str, usize> = BTreeMap::new();
    for (krate, f) in &sites {
        *per_crate.entry(krate).or_default() += 1;
        *per_callee.entry(f.callee.as_str()).or_default() += 1;
    }
    let n = sites.len();
    let listing = sites
        .iter()
        .map(|(_, f)| f.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    println!("env reads outside the core: {n}\nper crate: {per_crate:?}\nper callee: {per_callee:?}\n{listing}");
    assert!(
        n <= ENV_READ_BASELINE,
        "env reads rose to {n} (baseline {ENV_READ_BASELINE}). Read the value through \
         apprafter_core::Context instead. Sites:\n{listing}"
    );
    assert!(
        n == ENV_READ_BASELINE,
        "env reads fell to {n}: lower ENV_READ_BASELINE in this file to {n} in the same commit"
    );
}

#[test]
fn only_the_secret_store_reads_credentials_and_opens_caches() {
    let src = crate_dir("apprafter-core").join("src");
    let secrets = src.join("secrets");
    let hits: Vec<Finding> = scan_tree(&src)
        .into_iter()
        .filter(|f| match f.kind {
            Kind::SecretDoor => !f.file.starts_with(&secrets),
            Kind::ForeignRecipient => true,
            _ => false,
        })
        .collect();
    assert!(
        hits.is_empty(),
        "a target's credentials file, the age key and the caches are reached only through \
         apprafter-core/src/secrets/ (Context::secrets(), ADR 0068 §5), and a cache is sealed \
         only to a CacheKey's recipient:\n{}",
        hits.iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// T16 in the CLI (D.4a c2, d): `platform-cli/src` reaches a target's credentials file, the
/// shared age key and the caches only through `apprafter_core::secrets` — every door but the
/// seal is forbidden there (the shared key is created only by `SecretStore::new_cache`), and
/// every seal takes a `CacheKey`'s recipient in the same expression.
#[test]
fn the_cli_reaches_credentials_and_caches_only_through_the_secret_store() {
    // A door the core forbids outright is classified by FORBIDDEN_CALLEES first
    // (`load_or_create_identity`: in the core a read never writes a key), so it comes back as a
    // ForbiddenCallee under its entry; in the CLI it is a door like the others.
    let door = |f: &Finding| {
        f.kind == Kind::SecretDoor
            || (f.kind == Kind::ForbiddenCallee && SECRET_DOORS.contains(&f.callee.as_str()))
    };
    let findings = scan_tree(&crate_dir("platform-cli").join("src"));
    let hits: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            if door(f) {
                f.callee != SEAL
            } else {
                f.kind == Kind::ForeignRecipient
            }
        })
        .collect();
    assert!(
        hits.is_empty(),
        "the CLI reaches a credentials file, the age key or a cache only through \
         apprafter_core::secrets (Context::secrets(), ADR 0068 section 5), and seals a cache \
         only to a CacheKey's recipient:\n{}",
        hits.iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// c1's hand-off, T16: a vault is read, written, parsed and opened only by the secret store,
/// the lock flows and the vault module — never by another core module, the CLI or the desktop;
/// and only the secret store and the lock flows move a credentials file, a clear token or a
/// cache through `secrets::transfer` (critic finding 9).
#[test]
fn only_the_secret_store_and_the_lock_open_a_vault() {
    let src = crate_dir("apprafter-core").join("src");
    let allowed = [src.join("secrets"), src.join("lock"), src.join("vault")];
    let transfer_allowed = [src.join("secrets"), src.join("lock")];
    let desktop = cli_dir().parent().expect("the repo").join("desktop");
    let mut hits: Vec<Finding> = scan_tree(&src)
        .into_iter()
        .filter(|f| match f.kind {
            Kind::VaultDoor => !allowed.iter().any(|a| f.file.starts_with(a)),
            Kind::TransferDoor => !transfer_allowed.iter().any(|a| f.file.starts_with(a)),
            _ => false,
        })
        .collect();
    for other in [
        crate_dir("platform-cli").join("src"),
        desktop.join("src-tauri").join("src"),
        desktop.join("ipc").join("src"),
    ] {
        hits.extend(
            scan_tree(&other)
                .into_iter()
                .filter(|f| f.kind == Kind::VaultDoor),
        );
    }
    assert!(
        hits.is_empty(),
        "a vault is opened only through apprafter_core::lock and Context::secrets():\n{}",
        hits.iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every identifier of `tokens` with its line, comments and strings left out (proc-macro2
/// turns a doc comment into a string literal).
fn idents(tokens: TokenStream, out: &mut Vec<(String, usize)>) {
    for t in tokens {
        match t {
            TokenTree::Ident(i) => out.push((i.to_string(), i.span().start().line)),
            TokenTree::Group(g) => idents(g.stream(), out),
            _ => {}
        }
    }
}

/// L6 (critic G12): the CLI and the desktop never name a lock flow's fault or test policy.
#[test]
fn the_cli_and_the_desktop_never_arm_a_lock_fault() {
    let desktop = cli_dir().parent().expect("the repo").join("desktop");
    let mut hits = Vec::new();
    for dir in [
        crate_dir("platform-cli").join("src"),
        crate_dir("platform-cli").join("tests"),
        desktop.join("src-tauri").join("src"),
        desktop.join("src-tauri").join("tests"),
        desktop.join("ipc").join("src"),
        desktop.join("ipc").join("tests"),
    ] {
        for file in rust_files(&dir) {
            let src = fs::read_to_string(&file).unwrap();
            let tokens: TokenStream = src
                .parse()
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            let mut found = Vec::new();
            idents(tokens, &mut found);
            for (ident, line) in found {
                if TEST_ONLY_LOCK_KNOBS.contains(&ident.as_str()) {
                    hits.push(format!("{}:{line}: {ident}", file.display()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "only an in-process core test arms a lock fault or a weak KDF policy (L6):\n{}",
        hits.join("\n")
    );
}

/// 9 (WI-487): every write of a target's `state.json` in the core and the CLI goes through
/// `State::update` — under the store lock, from the file read again — never a `save` of a copy
/// the writer holds.
#[test]
fn every_state_json_write_goes_through_state_update() {
    let mut hits = Vec::new();
    for krate in ["apprafter-core", "platform-cli"] {
        hits.extend(
            scan_tree(&crate_dir(krate).join("src"))
                .into_iter()
                .filter(|f| f.kind == Kind::StateSave),
        );
    }
    assert!(
        hits.is_empty(),
        "state.json is written only through cli_state::State::update, under the store lock \
         (WI-487):\n{}",
        hits.iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn holder_endpoints_are_spelled_only_where_listed() {
    let mut hits = Vec::new();
    for krate in RATCHETED_CRATES.iter().chain(&["apprafter-core"]) {
        hits.extend(
            scan_tree(&crate_dir(krate).join("src"))
                .into_iter()
                .filter(|f| f.kind == Kind::EndpointLiteral),
        );
    }
    assert!(
        hits.is_empty(),
        "a per-user runtime path or a pipe name outside ENDPOINT_LITERALS (D.4a T8, G2): the \
         holder's endpoint is derived in one function from Context inputs, never from a uid or a \
         fixed pipe prefix:\n{}",
        hits.iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---------------------------------------------------------------------
// G3: real-environment contexts in tests (D.4a T6, T8)
// ---------------------------------------------------------------------

/// Every member of the `cli/` workspace: G3 reads the test code of each.
const CLI_MEMBERS: &[&str] = &[
    "platform-cli",
    "docsgen",
    "cli-core",
    "cli-state",
    "cli-providers",
    "apprafter-core",
    "backup-core",
    "apprafter-backup",
    "os-slots",
];

/// What builds a context from, or looks a location up in, the real environment of whoever runs
/// the tests, as the last segments of a value path (`a::Context::from_cli_env` matches
/// `Context::from_cli_env`); any `dirs::*` too.
const REAL_ENV_BUILDERS: &[&[&str]] = &[
    &["Context", "from_cli_env"],
    &["Context", "from_desktop_env"],
    &["default_config_root"],
    &["default_ssh_identity_path"],
    &["home_dir"],
    &["AllowListEnv", "from_process"],
    &["ProcessEnv"],
    &["cli_context"],
    // The context of `doctor` and `whoami`, which never asks for a secret (D.4a d).
    &["diagnostic_context"],
    // The CLI's context for a legacy arm, and the legacy commands' token, read through it
    // (D.4a b; `token.rs` since d4a-d).
    &["legacy_context"],
    &["token", "hetzner_token"],
    &["desktop_context"],
];

/// What reads or writes a store's secrets, a key or a cache, or runs a check that does: a test
/// that built its context from the real environment calls none of them, or it would read the
/// real store of whoever runs it. D.4a c2 adds the lock state's file, the marker and
/// `secrets::testing::locked_store`; d4a's later steps add the `lock`, `vault` and `holder`
/// openers; `Context::secrets` is a method, in [`OPENER_METHODS`].
const OPENERS: &[&[&str]] = &[
    &["load_identity"],
    &["load_or_create_identity"],
    &["load_or_create_identity_reporting"],
    &["save_target"],
    &["load_target"],
    &["materialise_kubeconfig"],
    &["cached_kubeconfig"],
    &["doctor", "run"],
    // D.4a c2: the lock state's file and the credentials marker.
    &["read_store_file"],
    &["write_store_file"],
    &["write_marker"],
    &["locked_store"],
    // D.4a d: the lock flows and the store's window read or write the store's secrets.
    &["plan_on"],
    &["execute_on"],
    &["open_at"],
    &["open_slot_at"],
    &["set_unlock_timeout"],
    &["plan_off"],
    &["execute_off"],
    &["materialise"],
    &["sweep_stale"],
];

/// Openers called as a method of the context, matched by the method's name (a syntax scan does
/// not know the receiver's type): `Context::secrets`, the one door to a store's secrets
/// (D.4a b). A path visitor never sees a method, so they are listed apart from [`OPENERS`].
const OPENER_METHODS: &[&str] = &["secrets"];

/// The test functions allowed a real-environment builder — each compares paths or values and
/// opens nothing — as (file under `cli/`, function; `Type::name` inside an `impl`). A listed
/// function that is not itself a `#[test]` (a helper) hands what it built on: a call to it by
/// path counts as a builder in its caller, which must then be listed too and open nothing
/// (critic finding 10; `token.rs`'s `with_env_token` is one, the desktop's `real_api_base`
/// another).
const REAL_ENV_TESTS: &[(&str, &str)] = &[
    // The context builders' own tests: what each reads, and how the two clients agree.
    (
        "apprafter-core/src/context.rs",
        "cli_env_reads_every_cli_input",
    ),
    (
        "apprafter-core/src/context.rs",
        "empty_cli_overrides_are_kept_as_the_cli_reads_them",
    ),
    (
        "apprafter-core/src/context.rs",
        "the_age_key_defaults_under_the_home_unless_overridden",
    ),
    (
        "apprafter-core/src/context.rs",
        "the_desktop_takes_tool_path_and_runtime_dir_from_its_host",
    ),
    (
        "apprafter-core/src/context.rs",
        "cli_env_reads_store_root_api_base_and_token",
    ),
    (
        "apprafter-core/src/context.rs",
        "cli_env_defaults_without_variables",
    ),
    (
        "apprafter-core/src/context.rs",
        "a_windows_cli_without_a_store_variable_runs_under_local_app_data",
    ),
    (
        "apprafter-core/src/context.rs",
        "an_empty_token_is_no_override",
    ),
    (
        "apprafter-core/src/context.rs",
        "an_empty_api_base_is_kept_as_the_cli_did",
    ),
    (
        "apprafter-core/src/context.rs",
        "the_desktop_ignores_every_cli_override_it_inherits",
    ),
    (
        "apprafter-core/src/context.rs",
        "the_desktop_store_root_defaults_like_the_cli",
    ),
    (
        "apprafter-core/src/context.rs",
        "a_release_build_ignores_an_inherited_non_loopback_api_base",
    ),
    (
        "apprafter-core/src/context.rs",
        "the_desktop_reads_only_its_allow_list",
    ),
    (
        "apprafter-core/src/context.rs",
        "a_test_build_honours_a_loopback_api_base_only",
    ),
    (
        "apprafter-core/src/context.rs",
        "a_test_build_still_ignores_the_token_override",
    ),
    (
        "apprafter-core/src/context.rs",
        "a_refused_api_base_never_shows_its_value",
    ),
    (
        "apprafter-core/src/context.rs",
        "a_secret_never_shows_in_debug",
    ),
    // D.4a T49: the CLI's context carries the holder's base it read from a MapEnv; opens
    // nothing.
    (
        "apprafter-core/src/context.rs",
        "the_clis_context_carries_its_holder_base_and_the_desktops_none",
    ),
    // D.4a T8 (d4a-g): the desktop's test-build fence, read from a MapEnv; compares paths and
    // opens nothing.
    (
        "apprafter-core/src/context.rs",
        "a_desktop_context_takes_the_sandbox_only_in_a_fast_kdf_test_build",
    ),
    // whoami pings with the stored token, not the CLI override: the override needs the CLI's
    // builder; the store is a tempdir it names.
    (
        "apprafter-core/src/session.rs",
        "no_ping_skips_and_a_ping_uses_the_stored_token_even_with_an_override",
    ),
    // The token and the SKU check under the CLI override: the override needs the CLI's
    // builder; the store is a tempdir the test names (critic finding 4).
    (
        "apprafter-core/src/target/read.rs",
        "the_token_is_the_cli_override_else_the_stored_one",
    ),
    // D.4a c2: HCLOUD_TOKEN set on a locked store (the store is made by a helper).
    (
        "apprafter-core/src/target/read.rs",
        "a_locked_store_never_takes_hcloud_token",
    ),
    (
        "apprafter-core/src/target/machine.rs",
        "the_cli_override_token_validates_the_sku",
    ),
    // The legacy resolvers' own tests.
    (
        "cli-core/src/target.rs",
        "default_config_root_points_at_user_config_dir_under_apprafter",
    ),
    (
        "cli-core/src/target.rs",
        "default_config_root_honours_apprafter_config_dir_env_override",
    ),
    (
        "cli-core/src/target.rs",
        "default_config_root_ignores_empty_env_override",
    ),
    (
        "cli-providers/src/hetzner_cloud/kubeconfig.rs",
        "default_ssh_identity_path_honours_env_override",
    ),
    // The wizard's token source compares against the override the CLI's builder read.
    (
        "platform-cli/src/commands/target_wizard.rs",
        "the_token_source_compares_against_the_cli_override_not_the_process_env",
    ),
    // The legacy commands' token: `HCLOUD_TOKEN` needs the CLI's builder, a MapEnv over a
    // tempdir store and home, built by the listed helper `with_env_token` for the three tests
    // below; opens nothing.
    ("platform-cli/src/token.rs", "with_env_token"),
    (
        "platform-cli/src/token.rs",
        "hcloud_token_outranks_the_stored_one_of_a_named_target",
    ),
    (
        "platform-cli/src/token.rs",
        "an_empty_hcloud_token_is_unset",
    ),
    (
        "platform-cli/src/token.rs",
        "hcloud_token_no_longer_passes_an_unknown_default",
    ),
    // The Windows release gate: every per-user default resolves under the profile.
    (
        "platform-cli/tests/windows_profile_paths_test.rs",
        "per_user_defaults_resolve_under_the_profile_whatever_home_says",
    ),
];

/// One builder or opener in test code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EnvUse {
    file: String,
    site: String,
    line: usize,
    opener: bool,
    what: String,
}

/// G3's scan of one file: the builders and openers in its test code, with the function each sits
/// in, and every function of its test code.
struct RealEnvScanner<'f> {
    file: &'f str,
    /// The names of the listed functions: a call to one counts as a builder (`REAL_ENV_TESTS`).
    helpers: &'f [&'f str],
    /// The whole file is test code (under `tests/`, `#![cfg(test)]`, an out-of-line test module).
    whole: bool,
    /// `cfg(test)` items around the current node.
    test_depth: usize,
    impl_types: Vec<String>,
    fns: Vec<String>,
    uses: Vec<EnvUse>,
    sites: HashSet<String>,
}

impl RealEnvScanner<'_> {
    fn in_test(&self) -> bool {
        self.whole || self.test_depth > 0
    }

    fn check(&mut self, segs: &[String], span: Span) {
        if !self.in_test() {
            return;
        }
        let ends = |pat: &[&str]| {
            segs.len() >= pat.len()
                && segs[segs.len() - pat.len()..]
                    .iter()
                    .zip(pat)
                    .all(|(a, b)| a == b)
        };
        let last = segs.last();
        let hit = if segs.len() >= 2 && segs[0] == "dirs" {
            Some((false, segs.join("::")))
        } else if let Some(p) = REAL_ENV_BUILDERS.iter().find(|p| ends(p)) {
            Some((false, p.join("::")))
        } else if let Some(h) = self.helpers.iter().find(|h| last.is_some_and(|l| l == **h)) {
            Some((false, format!("{h} (a listed helper)")))
        } else {
            OPENERS
                .iter()
                .find(|p| ends(p))
                .map(|p| (true, p.join("::")))
        };
        if let Some((opener, what)) = hit {
            self.uses.push(EnvUse {
                file: self.file.to_string(),
                site: self.fns.last().cloned().unwrap_or_default(),
                line: line_of(span),
                opener,
                what,
            });
        }
    }

    fn function(&mut self, name: String, f: impl FnOnce(&mut Self)) {
        if self.in_test() {
            self.sites.insert(name.clone());
        }
        self.fns.push(name);
        f(self);
        self.fns.pop();
    }

    /// A macro body that parses as neither expressions nor statements: every `a::b` run of
    /// identifiers not after a `.`.
    fn tokens(&mut self, tokens: TokenStream) {
        let tts: Vec<TokenTree> = tokens.into_iter().collect();
        let punct =
            |i: usize, c: char| matches!(tts.get(i), Some(TokenTree::Punct(p)) if p.as_char() == c);
        let mut i = 0;
        while i < tts.len() {
            match &tts[i] {
                TokenTree::Group(g) => {
                    self.tokens(g.stream());
                    i += 1;
                }
                TokenTree::Ident(id) => {
                    let after_dot = i > 0 && punct(i - 1, '.');
                    let mut segs = vec![id.to_string()];
                    let mut j = i + 1;
                    while punct(j, ':') && punct(j + 1, ':') {
                        match tts.get(j + 2) {
                            Some(TokenTree::Ident(next)) => {
                                segs.push(next.to_string());
                                j += 3;
                            }
                            _ => break,
                        }
                    }
                    if !after_dot {
                        self.check(&segs, id.span());
                    }
                    i = j;
                }
                _ => i += 1,
            }
        }
    }
}

impl<'ast> Visit<'ast> for RealEnvScanner<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        let test = usize::from(cfg_implies_test(item_attrs(item)));
        self.test_depth += test;
        visit::visit_item(self, item);
        self.test_depth -= test;
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        let test = usize::from(cfg_implies_test(impl_item_attrs(item)));
        self.test_depth += test;
        visit::visit_impl_item(self, item);
        self.test_depth -= test;
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let name = match &*i.self_ty {
            syn::Type::Path(tp) => tp
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.impl_types.push(name);
        visit::visit_item_impl(self, i);
        self.impl_types.pop();
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let owner = self.impl_types.last().cloned().unwrap_or_default();
        self.function(format!("{owner}::{}", f.sig.ident), |s| {
            visit::visit_impl_item_fn(s, f)
        });
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.function(f.sig.ident.to_string(), |s| visit::visit_item_fn(s, f));
    }

    fn visit_expr_path(&mut self, e: &'ast syn::ExprPath) {
        let segs: Vec<String> = e
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        if let Some(first) = e.path.segments.first() {
            self.check(&segs, first.ident.span());
        }
        visit::visit_expr_path(self, e);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let name = m.method.to_string();
        if self.in_test() && OPENER_METHODS.contains(&name.as_str()) {
            self.uses.push(EnvUse {
                file: self.file.to_string(),
                site: self.fns.last().cloned().unwrap_or_default(),
                line: line_of(m.method.span()),
                opener: true,
                what: name,
            });
        }
        visit::visit_expr_method_call(self, m);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Ok(args) = mac.parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated)
        {
            for e in &args {
                self.visit_expr(e);
            }
        } else if let Ok(stmts) = mac.parse_body_with(syn::Block::parse_within) {
            for s in &stmts {
                self.visit_stmt(s);
            }
        } else {
            self.tokens(mac.tokens.clone());
        }
    }
}

/// G3's scan of `src`, the file `file` (under `cli/`); `whole` when all of it is test code;
/// `helpers` the names whose call counts as a builder.
fn real_env_source(
    file: &str,
    src: &str,
    whole: bool,
    helpers: &[&str],
) -> (Vec<EnvUse>, HashSet<String>) {
    let parsed = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {file}: {e}"));
    let mut s = RealEnvScanner {
        file,
        helpers,
        whole: whole || cfg_implies_test(&parsed.attrs),
        test_depth: 0,
        impl_types: Vec::new(),
        fns: Vec::new(),
        uses: Vec::new(),
        sites: HashSet::new(),
    };
    s.visit_file(&parsed);
    (s.uses, s.sites)
}

/// G3 over the workspace: every use, and every test function per file.
fn real_env_scan() -> (Vec<EnvUse>, HashMap<String, HashSet<String>>) {
    let mut uses = Vec::new();
    let mut sites: HashMap<String, HashSet<String>> = HashMap::new();
    // Every listed name, by its last segment: a `#[test]` is never called, so only a helper's
    // name can match a call (REAL_ENV_TESTS).
    let helpers: Vec<&str> = REAL_ENV_TESTS
        .iter()
        .map(|&(_, site)| site.rsplit("::").next().unwrap_or(site))
        .collect();
    for member in CLI_MEMBERS {
        let krate = crate_dir(member);
        let src_files = rust_files(&krate.join("src"));
        // Out-of-line test modules, as the purity scan finds them.
        let (mut test_files, mut test_dirs) = (Vec::new(), Vec::new());
        for f in &src_files {
            let scan = scan_source(f, &fs::read_to_string(f).unwrap());
            test_files.extend(scan.test_module_files);
            test_dirs.extend(scan.test_module_dirs);
        }
        let tests_dir = krate.join("tests");
        let test_tree = if tests_dir.is_dir() {
            rust_files(&tests_dir)
        } else {
            Vec::new()
        };
        for f in src_files.iter().chain(&test_tree) {
            let whole = f.starts_with(&tests_dir)
                || test_files.contains(f)
                || test_dirs.iter().any(|d| f.starts_with(d));
            let rel = f
                .strip_prefix(cli_dir())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let (u, s) = real_env_source(&rel, &fs::read_to_string(f).unwrap(), whole, &helpers);
            uses.extend(u);
            sites.entry(rel).or_default().extend(s);
        }
    }
    uses.sort();
    (uses, sites)
}

#[test]
fn real_environment_contexts_in_tests_stay_where_listed_and_open_nothing() {
    let (uses, sites) = real_env_scan();
    let listed =
        |file: &str, site: &str| REAL_ENV_TESTS.iter().any(|(f, s)| *f == file && *s == site);
    let show = |u: &EnvUse| format!("{}:{}: `{}` in `{}`", u.file, u.line, u.what, u.site);
    let unlisted: Vec<String> = uses
        .iter()
        .filter(|u| !u.opener && !listed(&u.file, &u.site))
        .map(show)
        .collect();
    assert!(
        unlisted.is_empty(),
        "a test builds a context from the real environment outside REAL_ENV_TESTS (D.4a T6, \
         G3): build it with Context::for_desktop(<tempdir>) instead, or list the function if it \
         only compares paths:\n{}",
        unlisted.join("\n")
    );
    let opening: Vec<String> = uses
        .iter()
        .filter(|u| u.opener && listed(&u.file, &u.site))
        .map(show)
        .collect();
    assert!(
        opening.is_empty(),
        "a test that builds from the real environment also opens a secret, a key or a cache: it \
         would read the real store of whoever runs it:\n{}",
        opening.join("\n")
    );
    for (file, site) in REAL_ENV_TESTS {
        assert!(
            sites.get(*file).is_some_and(|s| s.contains(*site)),
            "REAL_ENV_TESTS names {file} `{site}`, which is no test function there: a rename \
             left the guard blind"
        );
        assert!(
            uses.iter()
                .any(|u| !u.opener && u.file == *file && u.site == *site),
            "REAL_ENV_TESTS names {file} `{site}`, which no longer builds from the real \
             environment: drop the entry"
        );
    }
}

// ---------------------------------------------------------------------
// The scanner's own tests
// ---------------------------------------------------------------------

mod scanner {
    use super::*;

    fn hits(src: &str) -> Vec<(Kind, String, usize)> {
        scan_source(Path::new("src/x.rs"), src)
            .findings
            .into_iter()
            .map(|f| (f.kind, f.callee, f.line))
            .collect()
    }

    fn read(callee: &str, line: usize) -> (Kind, String, usize) {
        (Kind::EnvRead, callee.to_string(), line)
    }

    #[test]
    fn code_after_an_inline_test_module_is_scanned() {
        let src = "\
#[cfg(test)]
mod tests {
    fn t() { let _ = std::env::var(\"A\"); }
}
fn after() { let _ = std::env::var(\"B\"); }
";
        assert_eq!(hits(src), vec![read("std::env::var", 5)]);
    }

    #[test]
    fn a_cfg_test_const_does_not_stop_the_scan() {
        let src = "\
#[cfg(test)]
const SEAM: u8 = 1;
fn f() { let _ = std::env::var_os(\"A\"); }
";
        assert_eq!(hits(src), vec![read("std::env::var_os", 3)]);
    }

    #[test]
    fn a_renamed_import_resolves_to_its_target() {
        let src = "use std::env::var as v; fn f(){ v(\"X\"); }";
        assert_eq!(hits(src), vec![read("std::env::var", 1)]);
    }

    #[test]
    fn a_local_binding_shadows_an_imported_name() {
        let src = "\
use std::env::var as v;
fn param(v: &str) { g(v); }
fn closure() { let c = |v: &str| g(v); c(\"\"); }
fn arm(x: Option<u8>) { match x { Some(v) => g(v), None => {} } v(\"A\"); }
fn init() { let v = v(\"B\"); g(v); }
fn iflet(x: Option<u8>) { if let Some(v) = x { g(v); } v(\"C\"); }
fn inner() { let v = 1; { use std::env::var as v; v(\"D\"); } g(v); }
";
        assert_eq!(
            hits(src),
            vec![
                read("std::env::var", 4),
                read("std::env::var", 5),
                read("std::env::var", 6),
                read("std::env::var", 7),
            ]
        );
    }

    #[test]
    fn a_plain_import_and_a_module_import_resolve() {
        let src = "\
use std::env::var;
use std::env;
fn f() { var(\"A\"); env::current_dir(); }
";
        assert_eq!(
            hits(src),
            vec![read("std::env::var", 3), read("std::env::current_dir", 3)]
        );
    }

    #[test]
    fn glob_imports_resolve() {
        let src = "\
use std::env::*;
use std::process::*;
use std::io::*;
fn f() { var(\"A\"); exit(1); stdout(); }
";
        let kinds: Vec<Kind> = hits(src).into_iter().map(|(k, _, _)| k).collect();
        assert_eq!(kinds, vec![Kind::EnvRead, Kind::ProcessEnd, Kind::Terminal]);
    }

    #[test]
    fn a_print_macro_is_caught_with_any_delimiter() {
        for src in [
            "fn f() { print!{\"x\"} }",
            "fn f() { println!(\"x\"); }",
            "fn f() { eprint![\"x\"]; }",
            "fn f() { std::eprintln!(\"x\"); }",
            "fn f() -> u8 { dbg!(1) }",
            "fn f(dbg: u8) -> u8 { dbg!(dbg) }",
        ] {
            let got = hits(src);
            assert_eq!(got.len(), 1, "{src}: {got:?}");
            assert_eq!(got[0].0, Kind::Print, "{src}");
        }
    }

    /// 9 (WI-487): a raw save of a `State` is caught as a method call and as a path, and not
    /// in a test module; `State::update` is not a save.
    #[test]
    fn a_state_save_is_caught_as_a_method_and_as_a_path() {
        let src = "\
use cli_state::State;
fn f(s: &State, p: &P) { s.save(p).unwrap(); State::save(s, p).unwrap(); State::update(p, h, c); }
#[cfg(test)]
mod tests { fn t(s: &cli_state::State, p: &P) { s.save(p).unwrap(); } }
";
        assert_eq!(
            hits(src),
            vec![
                (Kind::StateSave, ".save(…)".to_string(), 2),
                (Kind::StateSave, STATE_SAVE.to_string(), 2),
            ]
        );
    }

    #[test]
    fn a_cfg_all_test_item_is_skipped_and_a_cfg_any_test_item_is_not() {
        let src = "\
#[cfg(all(test, unix))]
fn only_in_tests() { let _ = std::env::var(\"A\"); }
#[cfg(any(test, unix))]
fn also_in_production() { let _ = std::env::var(\"B\"); }
#[cfg(not(test))]
fn production_only() { let _ = std::env::var(\"C\"); }
";
        assert_eq!(
            hits(src),
            vec![read("std::env::var", 4), read("std::env::var", 6)]
        );
    }

    #[test]
    fn env_and_option_env_are_compile_time_and_not_counted() {
        let src = "fn f() { let _ = env!(\"X\"); let _ = option_env!(\"Y\"); }";
        assert_eq!(hits(src), vec![]);
    }

    #[test]
    fn calls_inside_macro_arguments_are_scanned() {
        let src = "\
fn f() -> String { format!(\"{}\", std::env::var(\"A\").unwrap()) }
fn g() -> bool { matches!(std::env::var(\"B\"), Ok(ref v) if v == \"1\") }
";
        assert_eq!(
            hits(src),
            vec![read("std::env::var", 1), read("std::env::var", 2)]
        );
    }

    #[test]
    fn test_gated_impl_items_statements_and_imports_are_skipped() {
        let src = "\
#[cfg(test)]
use std::env::var;
struct T;
impl T {
    #[cfg(test)]
    fn t() { let _ = std::env::var(\"A\"); }
    fn p() {
        #[cfg(test)]
        let _ = std::env::var(\"B\");
        var(\"names nothing outside tests\");
        let _ = std::env::vars();
    }
}
#[cfg(test)]
impl T { fn u() { let _ = std::env::var(\"C\"); } }
";
        assert_eq!(hits(src), vec![read("std::env::vars", 11)]);
    }

    #[test]
    fn a_test_gated_loop_for_or_return_statement_is_skipped() {
        let src = "\
fn f() -> Option<String> {
    #[cfg(test)]
    loop { std::env::var(\"A\"); }
    #[cfg(test)]
    for _ in 0..1 { std::env::var(\"B\"); }
    #[cfg(test)]
    return std::env::var(\"C\").ok();
    std::env::var(\"D\").ok()
}
";
        assert_eq!(hits(src), vec![read("std::env::var", 8)]);
    }

    /// syn hangs a statement's attributes on its leftmost operand, not on
    /// the assignment, binary or cast expression the statement is.
    #[test]
    fn a_test_gated_assignment_binary_or_cast_statement_is_skipped() {
        let src = "\
fn f(mut x: Option<String>) -> bool {
    #[cfg(test)]
    x = std::env::var(\"A\").ok();
    #[cfg(test)]
    x.is_some() && std::env::var(\"B\").is_ok();
    #[cfg(test)]
    std::env::var(\"C\").is_ok() as u8;
    #[cfg(test)]
    std::env::var(\"D\").map(|v| v.len()).unwrap_or(0) as u32 + 1;
    #[cfg(test)]
    1 + 2 * 3 == std::env::var(\"E\").map(|v| v.len()).unwrap_or(0) as i32;
    std::env::var(\"F\").is_ok()
}
";
        assert_eq!(hits(src), vec![read("std::env::var", 12)]);
    }

    #[test]
    fn a_test_gated_match_arm_or_struct_field_is_skipped() {
        let src = "\
struct S { a: Option<String>, b: Option<String> }
fn f(x: u8) -> Option<String> {
    match x {
        #[cfg(test)]
        0 => std::env::var(\"A\").ok(),
        _ => std::env::var(\"B\").ok(),
    }
}
fn g() -> S {
    S {
        #[cfg(test)]
        a: std::env::var(\"C\").ok(),
        b: std::env::var(\"D\").ok(),
    }
}
";
        assert_eq!(
            hits(src),
            vec![read("std::env::var", 6), read("std::env::var", 13)]
        );
    }

    #[test]
    fn dirs_is_forbidden_but_a_local_named_dirs_is_not() {
        let src = "\
use dirs as d;
fn f(dirs: Vec<u8>) { dirs.len(); let _ = dirs::home_dir(); let _ = d::config_dir(); }
";
        assert_eq!(
            hits(src),
            vec![
                (Kind::Dirs, "dirs::home_dir".into(), 2),
                (Kind::Dirs, "dirs::config_dir".into(), 2),
            ]
        );
    }

    #[test]
    fn env_writes_process_ends_and_terminal_handles_are_caught() {
        let src = "\
fn f() {
    std::env::set_var(\"A\", \"1\");
    std::env::remove_var(\"A\");
    std::process::abort();
    let _ = std::io::stdin();
    let _ = ::std::io::stderr();
}
";
        let kinds: Vec<Kind> = hits(src).into_iter().map(|(k, _, _)| k).collect();
        assert_eq!(
            kinds,
            vec![
                Kind::EnvWrite,
                Kind::EnvWrite,
                Kind::ProcessEnd,
                Kind::Terminal,
                Kind::Terminal
            ]
        );
    }

    /// Review finding 3, and the `libc` / `windows-sys` dependencies `process::run_bounded`
    /// brought into the core: every other way it could reach the environment.
    #[test]
    fn the_other_environment_doors_the_core_can_reach_are_caught() {
        let src = "\
use windows_sys::Win32::System::Environment as winenv;
fn f(p: *mut i8) {
    let _ = std::env::home_dir();
    let _ = unsafe { libc::getenv(p) };
    let _ = unsafe { libc::secure_getenv(p) };
    let _ = winenv::GetEnvironmentVariableW(p, p, 0);
    std::env::set_current_dir(\"/\").unwrap();
    unsafe { libc::setenv(p, p, 1) };
    unsafe { libc::unsetenv(p) };
    unsafe { libc::putenv(p) };
    unsafe { libc::clearenv() };
    let _ = cli_providers::hetzner_cloud::swap_eligible_from_env(8);
    let _ = cli_providers::hetzner_cloud::user_data::swap_eligible_from_env(8);
}
";
        let got: Vec<(Kind, String)> = hits(src).into_iter().map(|(k, c, _)| (k, c)).collect();
        let k = |kind: Kind, c: &str| (kind, c.to_string());
        assert_eq!(
            got,
            vec![
                k(Kind::EnvRead, "std::env::home_dir"),
                k(Kind::EnvRead, "libc::getenv"),
                k(Kind::EnvRead, "libc::secure_getenv"),
                k(Kind::EnvRead, "windows_sys::Win32::System::Environment"),
                k(Kind::EnvWrite, "std::env::set_current_dir"),
                k(Kind::EnvWrite, "libc::setenv"),
                k(Kind::EnvWrite, "libc::unsetenv"),
                k(Kind::EnvWrite, "libc::putenv"),
                k(Kind::EnvWrite, "libc::clearenv"),
                k(
                    Kind::ForbiddenCallee,
                    "cli_providers::hetzner_cloud::swap_eligible_from_env"
                ),
                k(
                    Kind::ForbiddenCallee,
                    "cli_providers::hetzner_cloud::user_data::swap_eligible_from_env"
                ),
            ]
        );
    }

    #[test]
    fn forbidden_callees_resolve_through_reexports_and_imports() {
        let src = "\
use cli_core::resolve_hetzner_ssh_public_key as key;
use cli_providers::k8s::KubectlCli;
fn f() {
    let _ = key();
    let _k = KubectlCli;
    let _h = cli_providers::k8s::helm::HelmCli::default();
    let _l: cli_providers::k8s::HelmCli = Default::default();
    cli_core::logging::init();
}
";
        let got: Vec<(Kind, String)> = hits(src).into_iter().map(|(k, c, _)| (k, c)).collect();
        let fc = |c: &str| (Kind::ForbiddenCallee, c.to_string());
        assert_eq!(
            got,
            vec![
                fc("cli_core::resolve_hetzner_ssh_public_key"),
                fc("cli_providers::k8s::KubectlCli"),
                fc("cli_providers::k8s::helm::HelmCli"),
                fc("cli_providers::k8s::HelmCli"),
                fc("cli_core::logging::init"),
            ]
        );
    }

    #[test]
    fn the_sanctioned_callee_is_allowed_only_at_its_site() {
        let src = "\
struct Context;
impl Context {
    fn from_cli_env() { let _ = cli_core::target::config_root_from_override(None); }
    fn from_desktop_env() { let _ = cli_core::target::config_root_from_override(None); }
    fn for_desktop() { let _ = cli_core::target::config_root_from_override(None); }
}
";
        assert_eq!(
            hits(src),
            vec![(
                Kind::ForbiddenCallee,
                "cli_core::target::config_root_from_override".into(),
                5
            )]
        );
    }

    #[test]
    fn the_home_lookup_is_sanctioned_only_in_the_two_builders() {
        let src = "\
struct Context;
impl Context {
    fn from_cli_env() { let _ = cli_core::paths::home_dir(); }
    fn from_desktop_env() { let _ = cli_core::paths::home_dir(); }
    fn for_desktop() { let _ = cli_core::paths::home_dir(); }
}
";
        assert_eq!(
            hits(src),
            vec![(Kind::ForbiddenCallee, "cli_core::paths::home_dir".into(), 5)]
        );
    }

    #[test]
    fn a_secret_door_is_caught_through_imports_reexports_and_methods() {
        let src = "\
use cli_core::load_target as load;
use cli_core::target::save_target_credentials;
fn f(store: &cli_core::TargetStorePaths, id: &Identity) {
    let _ = load(store, \"a\");
    let _ = save_target_credentials(store, \"a\", &Default::default());
    let _ = cli_core::secrets::decrypt_with_identity(\"\", id);
    let _ = store.target_credentials_file(\"a\");
    let _ = cli_core::target::load_target_config(store, \"a\");
    let _ = format!(\"{}\", store.target_credentials_file(\"b\").display());
}
";
        let door = |c: &str, line| (Kind::SecretDoor, c.to_string(), line);
        assert_eq!(
            hits(src),
            vec![
                door("cli_core::load_target", 4),
                door("cli_core::target::save_target_credentials", 5),
                door("cli_core::secrets::decrypt_with_identity", 6),
                door("target_credentials_file", 7),
                door("target_credentials_file", 9),
            ]
        );
    }

    /// D.4a c2: the decrypt that says why is a door, like the one it extends.
    #[test]
    fn the_decrypt_that_says_why_is_a_secret_door() {
        let src = "fn f(k: &Identity) { let _ = cli_core::secrets::decrypt_with_identity_reason(\"a\", k); }";
        assert_eq!(
            hits(src),
            vec![(
                Kind::SecretDoor,
                "cli_core::secrets::decrypt_with_identity_reason".to_string(),
                1
            )]
        );
    }

    /// D.4a c2: the credentials file's bytes and their parse are doors, like the read they split.
    #[test]
    fn the_credentials_bytes_and_their_parse_are_secret_doors() {
        let src = "fn f(p: &TargetStorePaths) { let b = cli_core::target::read_target_credentials_bytes(p, \"x\"); let _ = cli_core::target::parse_target_credentials(p, \"x\", &b); }";
        let mut got = hits(src);
        got.sort();
        assert_eq!(
            got,
            vec![
                (
                    Kind::SecretDoor,
                    "cli_core::target::parse_target_credentials".to_string(),
                    1
                ),
                (
                    Kind::SecretDoor,
                    "cli_core::target::read_target_credentials_bytes".to_string(),
                    1
                ),
            ]
        );
    }

    #[test]
    fn a_seal_takes_a_cache_keys_recipient_inline() {
        let src = "\
use cli_core::secrets::encrypt_for_recipient as seal;
fn f(key: &CacheKey, other: &Identity) -> cli_core::Result<()> {
    seal(\"a\", &key.recipient()?)?;
    seal(\"b\", &other.to_public())?;
    let r = key.recipient()?;
    cli_core::secrets::encrypt_for_recipient(\"c\", &r)?;
    Ok(())
}
";
        let foreign: Vec<(Kind, String, usize)> = hits(src)
            .into_iter()
            .filter(|(k, _, _)| *k == Kind::ForeignRecipient)
            .collect();
        let seal = |line| (Kind::ForeignRecipient, SEAL.to_string(), line);
        assert_eq!(foreign, vec![seal(4), seal(6)]);
    }

    /// The vault's doors are seen through imports, `self` included.
    #[test]
    fn a_vault_door_is_seen_through_its_import() {
        let src = "\
use crate::vault::{self, VaultFile};
fn f(p: &std::path::Path, b: &[u8]) {
    let _ = vault::file::read(p);
    let _ = VaultFile::parse(b);
}
";
        let doors: Vec<String> = scan_source(Path::new("x.rs"), src)
            .findings
            .into_iter()
            .filter(|f| f.kind == Kind::VaultDoor)
            .map(|f| f.callee)
            .collect();
        assert_eq!(doors, vec!["crate::vault::file", "crate::vault::VaultFile"]);
    }

    /// D.4b B8: a client reads a vault's id through `secrets`, which is no vault door, while
    /// a door named beside it is still seen (a scanner that over- or under-matches fails
    /// this: critic round, finding 6).
    #[test]
    fn the_vault_id_on_disk_is_no_vault_door() {
        let src = "\
use apprafter_core::secrets::vault_id_on_disk;
use apprafter_core::vault::{self, VaultId};
fn f(p: &std::path::Path) -> Option<VaultId> {
    let _ = vault::file::read(p);
    vault_id_on_disk(p).ok().flatten()
}
";
        let doors: Vec<String> = scan_source(Path::new("x.rs"), src)
            .findings
            .into_iter()
            .filter(|f| f.kind == Kind::VaultDoor)
            .map(|f| f.callee)
            .collect();
        assert_eq!(doors, vec!["apprafter_core::vault::file"]);
    }

    /// The lock flows' file doors are seen through `{self, …}` imports too (critic finding 9).
    #[test]
    fn a_transfer_door_is_seen_through_its_import() {
        let src = "\
use crate::secrets::transfer::{self, Credentials};
fn f(s: &cli_core::target::TargetStorePaths) {
    let _ = transfer::credentials(s, \"prod\");
}
";
        let doors: Vec<String> = scan_source(Path::new("x.rs"), src)
            .findings
            .into_iter()
            .filter(|f| f.kind == Kind::TransferDoor)
            .map(|f| f.callee)
            .collect();
        assert_eq!(doors, vec!["crate::secrets::transfer"]);
    }

    /// L6's scan: a knob named in a doc comment or a string is no use of it.
    #[test]
    fn a_knob_named_in_a_comment_or_a_string_is_not_a_use() {
        let mut found = Vec::new();
        idents(
            "/// FaultPoint\nfn f() {\n    let _ = \"with_fault\";\n    g::with_fault();\n}\n"
                .parse()
                .unwrap(),
            &mut found,
        );
        let knobs: Vec<(String, usize)> = found
            .into_iter()
            .filter(|(i, _)| TEST_ONLY_LOCK_KNOBS.contains(&i.as_str()))
            .collect();
        assert_eq!(knobs, vec![("with_fault".to_string(), 4)]);
    }

    #[test]
    fn ts_export_is_forbidden_in_the_core() {
        for (src, bad) in [
            ("#[derive(ts_rs::TS)] #[ts(export)] struct A;", true),
            ("#[ts(rename = \"B\", export)] struct B;", true),
            (
                "#[cfg_attr(feature = \"ts\", ts(export_to = \"x.ts\"))] struct C;",
                true,
            ),
            (
                "#[cfg_attr(feature = \"ts\", derive(ts_rs::TS))] struct D;",
                false,
            ),
            ("#[ts(rename = \"export\")] struct E;", false),
            // syn's `MetaList` takes any delimiter, and ts-rs honours each.
            ("#[ts{export}] struct F;", true),
            ("#[ts[export]] struct G;", true),
            ("#[cfg_attr(feature = \"ts\", ts{export})] struct H;", true),
        ] {
            assert_eq!(!ts_exports(src).is_empty(), bad, "{src}");
        }
    }

    #[test]
    fn a_direct_ts_rs_export_call_is_forbidden_in_the_core() {
        for (src, bad) in [
            ("UiError::export_all(&cfg)", true),
            ("<UiError as ts_rs::TS>::export(&cfg).unwrap();", true),
            ("ts_rs::TS::export_all_to(\"out\")", true),
            ("fn export_report() {}", false),
            ("report.export()", false),
            ("let s = \"::export\";", false),
        ] {
            assert_eq!(!ts_exports(src).is_empty(), bad, "{src}");
        }
    }

    #[test]
    fn the_ts_export_scan_covers_the_crates_tests_too() {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, body: &str| {
            let p = dir.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        };
        write("src/lib.rs", "pub struct A;\n");
        write(
            "tests/ipc.rs",
            "#[test]\nfn t() {\n    A::export_all(&cfg).unwrap();\n}\n",
        );
        let got: Vec<(PathBuf, usize)> = ts_export_crate(dir.path())
            .into_iter()
            .map(|f| (f.file, f.line))
            .collect();
        assert_eq!(got, vec![(dir.path().join("tests/ipc.rs"), 3)]);
    }

    #[test]
    fn an_out_of_line_test_module_and_its_children_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let write = |rel: &str, body: &str| {
            let p = src.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        };
        let read_env = "fn f() { let _ = std::env::var(\"A\"); }";
        write(
            "lib.rs",
            "#[cfg(test)]\nmod tests;\nmod prod;\nmod nested;\n",
        );
        write("tests.rs", read_env);
        write("prod.rs", read_env);
        write("nested.rs", "#[cfg(test)]\nmod deep;\nmod shallow;\n");
        write("nested/deep.rs", read_env);
        write("nested/deep/more.rs", read_env);
        write("nested/shallow.rs", read_env);
        let files: Vec<PathBuf> = scan_tree(&src).into_iter().map(|f| f.file).collect();
        assert_eq!(
            files,
            vec![src.join("nested/shallow.rs"), src.join("prod.rs")]
        );
    }

    #[test]
    fn the_dependency_guard_sees_renames_targets_and_workspace_entries() {
        let member = r#"
[dependencies]
prompt  = { package = "inquire", version = "0.7" }
colours = { workspace = true }
serde   = { workspace = true }

[target.'cfg(windows)'.dependencies]
ctrlc = "3"

[dev-dependencies]
indicatif = "0.17"
"#;
        let workspace = r#"
[workspace.dependencies]
colours = { package = "owo-colors", version = "4" }
serde   = "1"
"#;
        let mut got = forbidden_deps(member, workspace);
        got.sort();
        assert_eq!(got, vec!["ctrlc", "inquire", "owo-colors"]);
    }

    #[test]
    fn confstr_is_an_env_read() {
        // A per-user lookup that ignores the environment (macOS `_CS_DARWIN_USER_TEMP_DIR`).
        let src = "fn f() { unsafe { libc::confstr(65537, std::ptr::null_mut(), 0); } }";
        assert_eq!(hits(src), vec![read("libc::confstr", 1)]);
    }

    #[test]
    fn a_runtime_path_or_a_pipe_name_is_an_endpoint_literal() {
        let src = r###"
fn f() {
    let _ = "/run/user/1000/apprafter";
    let _ = r"\\.\pipe\apprafter-x";
    let _ = format!("/run/user/{}", 1);
}
#[doc = "/run/user/1000 in a doc comment is no endpoint"]
fn g() { let _ = "/var/run/apprafter"; }
"###;
        let got: Vec<(Kind, usize)> = hits(src)
            .into_iter()
            .map(|(kind, _, line)| (kind, line))
            .collect();
        assert_eq!(
            got,
            vec![
                (Kind::EndpointLiteral, 3),
                (Kind::EndpointLiteral, 4),
                (Kind::EndpointLiteral, 5),
            ]
        );
    }
}

// ---------------------------------------------------------------------
// G3's scanner, on sources of its own
// ---------------------------------------------------------------------

mod real_env {
    use super::*;

    fn uses_with(src: &str, whole: bool, helpers: &[&str]) -> Vec<(String, bool, String)> {
        real_env_source("x.rs", src, whole, helpers)
            .0
            .into_iter()
            .map(|u| (u.site, u.opener, u.what))
            .collect()
    }

    fn uses(src: &str, whole: bool) -> Vec<(String, bool, String)> {
        uses_with(src, whole, &[])
    }

    fn row(site: &str, opener: bool, what: &str) -> (String, bool, String) {
        (site.to_string(), opener, what.to_string())
    }

    #[test]
    fn a_builder_and_an_opener_in_a_test_module_are_seen_with_their_function() {
        let src = "\
fn production() { let _ = Context::from_cli_env(&ProcessEnv); }
#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        let ctx = apprafter_core::Context::from_cli_env(&MapEnv::new()).unwrap();
        let _ = crate::runtime::materialise_kubeconfig(&ctx, &target);
    }
}
";
        assert_eq!(
            uses(src, false),
            vec![
                row("t", false, "Context::from_cli_env"),
                row("t", true, "materialise_kubeconfig"),
            ]
        );
    }

    #[test]
    fn a_whole_test_file_counts_and_macro_arguments_are_read() {
        let src = "fn t() { assert_eq!(cli_core::target::default_config_root(), p); let _ = dirs::home_dir(); }";
        assert_eq!(
            uses(src, true),
            vec![
                row("t", false, "default_config_root"),
                row("t", false, "dirs::home_dir"),
            ]
        );
    }

    #[test]
    fn methods_types_and_imports_are_no_builders() {
        let src = "\
use cli_core::paths::home_dir;
struct ProcessEnv;
impl EnvSource for ProcessEnv {}
fn t(ctx: &Context) -> ProcessEnv { let _ = ctx.home_dir(); todo!() }
";
        assert_eq!(uses(src, true), Vec::<(String, bool, String)>::new());
    }

    #[test]
    fn a_method_is_named_with_its_type() {
        let src =
            "#[cfg(test)]\nmod tests { struct S; impl S { fn build() { let _ = ProcessEnv; } } }";
        assert_eq!(uses(src, false), vec![row("S::build", false, "ProcessEnv")]);
    }

    /// D.4a b: the secret store is an opener, called as a method of the context.
    #[test]
    fn the_secret_store_is_an_opener_called_as_a_method() {
        let src = "\
fn t() {
    let ctx = Context::from_cli_env(&MapEnv::new()).unwrap();
    let _ = ctx.secrets().stored_token(\"p\");
    assert!(ctx.secrets().held().is_ok());
    let _ = ctx.store();
}
";
        assert_eq!(
            uses(src, true),
            vec![
                row("t", false, "Context::from_cli_env"),
                row("t", true, "secrets"),
                row("t", true, "secrets"),
            ]
        );
    }

    /// D.4a c2: the lock state's file and the marker are openers: a test that built its context
    /// from the real environment and then read or wrote them would reach the real store.
    #[test]
    fn the_lock_files_are_openers() {
        let src = "\
fn t() {
    let ctx = Context::from_cli_env(&MapEnv::new()).unwrap();
    let _ = apprafter_core::secrets::read_store_file(ctx.config_root());
    let _ = write_store_file(ctx.config_root(), &file, &held);
    let _ = marker::write_marker(&ctx.store(), \"p\", &id);
}
";
        assert_eq!(
            uses(src, true),
            vec![
                row("t", false, "Context::from_cli_env"),
                row("t", true, "read_store_file"),
                row("t", true, "write_store_file"),
                row("t", true, "write_marker"),
            ]
        );
    }

    /// D.4a c2: the test store is an opener: it writes a vault into the store it is given.
    #[test]
    fn the_test_store_is_an_opener() {
        let src = "fn t() { let ctx = Context::from_cli_env(&MapEnv::new()).unwrap(); let _ = secrets::testing::locked_store(ctx.config_root(), &[]); }";
        assert_eq!(
            uses(src, true),
            vec![
                row("t", false, "Context::from_cli_env"),
                row("t", true, "locked_store"),
            ]
        );
    }

    /// D.4a b, d: the CLI's legacy context and the legacy commands' token build from the real
    /// environment; the core's own `target::hetzner_token` does not (it takes a context).
    #[test]
    fn the_legacy_context_and_the_legacy_token_are_builders() {
        let src = "\
fn t() {
    let _ = crate::token::hetzner_token(None);
    let _ = crate::context::legacy_context();
    let _ = apprafter_core::target::hetzner_token(&ctx, &target);
}
";
        assert_eq!(
            uses(src, true),
            vec![
                row("t", false, "token::hetzner_token"),
                row("t", false, "legacy_context"),
            ]
        );
    }

    /// Critic finding 10: a listed helper hands what it built on, so a call to it (by path:
    /// `real_ctx()`, `Self::real_ctx()`, `S::real_ctx()`) counts as a builder in its caller.
    #[test]
    fn a_call_to_a_listed_helper_counts_as_a_builder() {
        let src = "\
fn t() {
    let ctx = real_ctx();
    let _ = cli_core::load_target(&ctx.store(), \"x\");
}
fn u() { let _ = Self::real_ctx(); let _ = other(); }
";
        assert_eq!(
            uses_with(src, true, &["real_ctx"]),
            vec![
                row("t", false, "real_ctx (a listed helper)"),
                row("t", true, "load_target"),
                row("u", false, "real_ctx (a listed helper)"),
            ]
        );
        assert_eq!(uses(src, true), vec![row("t", true, "load_target")]);
    }
}

// ---------------------------------------------------------------------
// L4 (D.4a T23): the test-only key-derivation feature stays in tests
// ---------------------------------------------------------------------

/// The `apprafter-core` feature that makes `vault::KdfPolicy::active()` the test-only policy:
/// Argon2id parameters a release build refuses.
const INSECURE_KDF_FEATURE: &str = "insecure-fast-kdf";

/// `(manifest, feature)` pairs allowed to forward it: the desktop's test build (d4a-g adds the
/// edge; until then the entry names a feature that forwards nothing). Each entry must name a
/// feature that exists.
const INSECURE_KDF_FORWARDERS: &[(&str, &str)] = &[("desktop/src-tauri/Cargo.toml", "test-build")];

/// A feature that must stay in tests: the package that declares it, its name, and the
/// `(manifest, feature)` pairs allowed to forward it (each must name a feature that exists).
struct FeatureRule {
    package: &'static str,
    feature: &'static str,
    forwarders: &'static [(&'static str, &'static str)],
}

/// L4 (D.4a T23).
const INSECURE_KDF: FeatureRule = FeatureRule {
    package: "apprafter-core",
    feature: INSECURE_KDF_FEATURE,
    forwarders: INSECURE_KDF_FORWARDERS,
};

/// G-OS2 (D.4b B43): the os-slots stand-ins (`ScriptedAccountPassword`) reach no shipped build.
/// `platform-cli`'s `test-os-slots` (d4b-e1a) and the desktop's test build (d4b-g1b) join
/// `forwarders` when they forward it; e1a adds the rule for `test-os-slots` itself.
const OS_SLOTS_TEST_SUPPORT: FeatureRule = FeatureRule {
    package: "apprafter-os-slots",
    feature: "test-support",
    forwarders: &[],
};

/// L4's rule over one manifest.
fn insecure_kdf_edges(file: &str, manifest: &str) -> Vec<String> {
    feature_edges(&INSECURE_KDF, file, manifest)
}

/// Where a manifest turns `rule`'s feature on outside `[dev-dependencies]`: a dependency edge on
/// its package that lists it (in `[dependencies]`, `[build-dependencies]`,
/// `[workspace.dependencies]` or a target table; renamed or not), a feature that forwards it
/// (other than the rule's forwarders), or — in the package's own manifest — another feature,
/// `default` included, that pulls it in.
fn feature_edges(rule: &FeatureRule, file: &str, manifest: &str) -> Vec<String> {
    let doc: toml::Value = manifest
        .parse()
        .unwrap_or_else(|e| panic!("{file} does not parse: {e}"));
    let is_owner = doc
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        == Some(rule.package);
    let mut hits = Vec::new();
    for (place, table) in dependency_tables(&doc, false) {
        for (name, spec) in table {
            let package = spec.get("package").and_then(|p| p.as_str()).unwrap_or(name);
            let enables = spec
                .get("features")
                .and_then(|f| f.as_array())
                .is_some_and(|list| list.iter().any(|f| f.as_str() == Some(rule.feature)));
            if package == rule.package && enables {
                hits.push(format!(
                    "{file}: [{place}] `{name}` enables {}",
                    rule.feature
                ));
            }
        }
    }
    if let Some(features) = doc.get("features").and_then(|f| f.as_table()) {
        let forwarded = [
            format!("{}/{}", rule.package, rule.feature),
            format!("{}?/{}", rule.package, rule.feature),
        ];
        for (feature, list) in features {
            let forwards = list.as_array().into_iter().flatten().any(|item| {
                let item = item.as_str().unwrap_or_default();
                forwarded.iter().any(|f| f == item) || (is_owner && item == rule.feature)
            });
            let allowed = rule
                .forwarders
                .iter()
                .any(|(f, feat)| *f == file && *feat == feature.as_str());
            if forwards && !allowed {
                hits.push(format!(
                    "{file}: feature `{feature}` enables {}",
                    rule.feature
                ));
            }
        }
    }
    hits
}

/// A manifest's dependency tables as `(place, table)`: `[dependencies]`,
/// `[build-dependencies]`, with `dev` also `[dev-dependencies]`, their target forms, and
/// `[workspace.dependencies]`.
fn dependency_tables(doc: &toml::Value, dev: bool) -> Vec<(String, &toml::value::Table)> {
    let kinds: &[&str] = if dev {
        &["dependencies", "build-dependencies", "dev-dependencies"]
    } else {
        &["dependencies", "build-dependencies"]
    };
    let mut tables = Vec::new();
    for key in kinds {
        if let Some(table) = doc.get(*key).and_then(|t| t.as_table()) {
            tables.push(((*key).to_string(), table));
        }
    }
    if let Some(table) = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|t| t.as_table())
    {
        tables.push(("workspace.dependencies".to_string(), table));
    }
    if let Some(targets) = doc.get("target").and_then(|t| t.as_table()) {
        for (cfg, target) in targets {
            for key in kinds {
                if let Some(table) = target.get(*key).and_then(|t| t.as_table()) {
                    tables.push((format!("target.{cfg}.{key}"), table));
                }
            }
        }
    }
    tables
}

/// Every `Cargo.toml` under `cli/` and `desktop/` (as [`manifests_under`] finds them), as
/// (its path from the repository root, its text).
fn every_manifest() -> Vec<(String, String)> {
    let repo = cli_dir()
        .parent()
        .expect("cli/ sits in the repository")
        .to_path_buf();
    let mut paths = Vec::new();
    for dir in ["cli", "desktop"] {
        manifests_under(&repo.join(dir), &mut paths);
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(&repo)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
            (rel, text)
        })
        .collect()
}

/// Every `Cargo.toml` under `dir`, build output, `node_modules` and dot-directories left out.
fn manifests_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
        let path = entry.unwrap().path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if !(name == "target" || name == "node_modules" || name.starts_with('.')) {
                manifests_under(&path, out);
            }
        } else if name == "Cargo.toml" {
            out.push(path);
        }
    }
}

#[test]
fn only_the_desktop_test_build_may_turn_on_the_insecure_kdf() {
    let repo = cli_dir()
        .parent()
        .expect("cli/ sits in the repository")
        .to_path_buf();
    let mut manifests = Vec::new();
    for dir in ["cli", "desktop"] {
        manifests_under(&repo.join(dir), &mut manifests);
    }
    manifests.sort();
    assert!(
        manifests.len() >= 13,
        "found only {} manifests under cli/ and desktop/: {manifests:?}",
        manifests.len()
    );
    let mut hits = Vec::new();
    for path in &manifests {
        let rel = path
            .strip_prefix(&repo)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        hits.extend(insecure_kdf_edges(&rel, &text));
    }
    assert!(
        hits.is_empty(),
        "the test-only key derivation must never reach a shipped build (D.4a T23, L4):\n{}",
        hits.join("\n")
    );
    let core: toml::Value = fs::read_to_string(crate_dir("apprafter-core").join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        core.get("features")
            .and_then(|f| f.get(INSECURE_KDF_FEATURE))
            .is_some(),
        "apprafter-core declares no `{INSECURE_KDF_FEATURE}` feature: this guard would see nothing"
    );
    for (file, feature) in INSECURE_KDF_FORWARDERS {
        let doc: toml::Value = fs::read_to_string(repo.join(file))
            .unwrap_or_else(|e| panic!("read {file}: {e}"))
            .parse()
            .unwrap();
        assert!(
            doc.get("features").and_then(|f| f.get(*feature)).is_some(),
            "{file} has no feature `{feature}`: a stale allow-list entry"
        );
    }
}

mod insecure_kdf {
    use super::*;

    fn edges(file: &str, manifest: &str) -> usize {
        insecure_kdf_edges(file, manifest).len()
    }

    #[test]
    fn a_dev_dependency_may_enable_it() {
        let m = "[dev-dependencies]\napprafter-core = { path = \"x\", features = [\"insecure-fast-kdf\"] }\n";
        assert_eq!(edges("cli/x/Cargo.toml", m), 0);
    }

    #[test]
    fn every_other_edge_is_refused() {
        for m in [
            "[dependencies]\napprafter-core = { path = \"x\", features = [\"insecure-fast-kdf\"] }\n",
            "[build-dependencies]\napprafter-core = { path = \"x\", features = [\"insecure-fast-kdf\"] }\n",
            "[dependencies]\ncore = { package = \"apprafter-core\", path = \"x\", features = [\"insecure-fast-kdf\"] }\n",
            "[target.'cfg(unix)'.dependencies]\napprafter-core = { path = \"x\", features = [\"insecure-fast-kdf\"] }\n",
            "[workspace.dependencies]\napprafter-core = { path = \"x\", features = [\"insecure-fast-kdf\"] }\n",
            "[features]\nfast = [\"apprafter-core/insecure-fast-kdf\"]\n",
            "[features]\nfast = [\"apprafter-core?/insecure-fast-kdf\"]\n",
        ] {
            assert_eq!(edges("cli/x/Cargo.toml", m), 1, "{m}");
        }
    }

    #[test]
    fn the_core_pulls_it_in_from_no_feature_of_its_own() {
        let core = "[package]\nname = \"apprafter-core\"\n[features]\ninsecure-fast-kdf = []\n";
        assert_eq!(edges("cli/apprafter-core/Cargo.toml", core), 0);
        for extra in [
            "default = [\"insecure-fast-kdf\"]\n",
            "test-support = [\"insecure-fast-kdf\"]\n",
        ] {
            assert_eq!(
                edges("cli/apprafter-core/Cargo.toml", &format!("{core}{extra}")),
                1,
                "{extra}"
            );
        }
    }

    #[test]
    fn only_the_desktop_test_build_may_forward_it() {
        let m = "[features]\ntest-build = [\"apprafter-core/insecure-fast-kdf\"]\n";
        assert_eq!(edges("desktop/src-tauri/Cargo.toml", m), 0);
        assert_eq!(edges("desktop/ipc/Cargo.toml", m), 1);
        let other = "[features]\nwalk = [\"apprafter-core/insecure-fast-kdf\"]\n";
        assert_eq!(edges("desktop/src-tauri/Cargo.toml", other), 1);
    }
}

// ---------------------------------------------------------------------
// The vault's code rules (D.4a T17, T29; crypto brief §10, §13 mutant 2)
// ---------------------------------------------------------------------

/// Names under `src/vault/` that hold key material or what it is compared with. An `==` or
/// `!=` with one of them in an operand compares a secret in variable time: the vault compares
/// them with `subtle::ConstantTimeEq::ct_eq`.
const VAULT_SECRET_NAMES: &[&str] = &[
    "key_check",
    "kek",
    "data_key",
    "material",
    "expose",
    "key_bytes",
];

/// argon2's methods that bring their own working memory and free it unzeroed (argon2 0.6.0,
/// `src/block.rs:190-200`): the vault calls `hash_password_into_with_memory` with a
/// `Zeroizing` buffer instead.
const UNZEROED_ARGON2: &[&str] = &[
    "hash_password_into",
    "hash_password",
    "hash_password_customized",
];

/// The vault's constant-time comparisons today: a slot's key check (seal.rs), and the recovery
/// key's canonical-text check and confirmation (recovery.rs). Fewer means one became `==`
/// under a name this guard does not list.
const VAULT_CT_EQ_MIN: usize = 3;

#[derive(Default)]
struct VaultRules {
    file: String,
    findings: Vec<String>,
    ct_eq: usize,
}

impl VaultRules {
    fn hit(&mut self, span: Span, what: &str) {
        self.findings
            .push(format!("{}:{}: {what}", self.file, line_of(span)));
    }
}

/// Whether `e` names one of [`VAULT_SECRET_NAMES`] anywhere: a variable, a field, a method.
fn names_vault_secret(e: &Expr) -> bool {
    struct Find(bool);
    impl<'a> Visit<'a> for Find {
        fn visit_ident(&mut self, i: &'a proc_macro2::Ident) {
            if VAULT_SECRET_NAMES.contains(&i.to_string().as_str()) {
                self.0 = true;
            }
        }
    }
    let mut find = Find(false);
    find.visit_expr(e);
    find.0
}

impl<'ast> Visit<'ast> for VaultRules {
    fn visit_item(&mut self, item: &'ast Item) {
        if !cfg_implies_test(item_attrs(item)) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        if !cfg_implies_test(impl_item_attrs(item)) {
            visit::visit_impl_item(self, item);
        }
    }

    fn visit_expr_binary(&mut self, e: &'ast syn::ExprBinary) {
        if matches!(e.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_))
            && (names_vault_secret(&e.left) || names_vault_secret(&e.right))
        {
            let span = match &e.op {
                syn::BinOp::Eq(t) => t.spans[0],
                syn::BinOp::Ne(t) => t.spans[0],
                _ => unreachable!("matched Eq or Ne above"),
            };
            self.hit(
                span,
                "`==` / `!=` on key material: compare with `ConstantTimeEq::ct_eq`",
            );
        }
        visit::visit_expr_binary(self, e);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        let name = m.method.to_string();
        if name == "ct_eq" {
            self.ct_eq += 1;
        }
        if UNZEROED_ARGON2.contains(&name.as_str()) {
            self.hit(
                m.method.span(),
                &format!("argon2's `{name}` frees its working memory unzeroed"),
            );
        }
        visit::visit_expr_method_call(self, m);
    }

    fn visit_attribute(&mut self, a: &'ast Attribute) {
        if a.path().is_ident("derive") {
            if let Meta::List(list) = &a.meta {
                let derives = list.tokens.to_string();
                if derives.contains("Serialize") || derives.contains("Deserialize") {
                    self.hit(
                        a.pound_token.spans[0],
                        "a vault type derives Serialize or Deserialize",
                    );
                }
            }
        }
        visit::visit_attribute(self, a);
    }
}

/// The findings and the `ct_eq` count of one source file of `src/vault/`.
fn vault_rules_source(file: &str, src: &str) -> (Vec<String>, usize) {
    let parsed = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {file}: {e}"));
    let mut rules = VaultRules {
        file: file.to_string(),
        ..VaultRules::default()
    };
    rules.visit_file(&parsed);
    (rules.findings, rules.ct_eq)
}

/// The production files of `src/vault/`: every `.rs` but those of a `#[cfg(test)] mod x;` in
/// `vault/mod.rs`.
fn vault_production_files(dir: &Path) -> Vec<PathBuf> {
    let mod_rs = fs::read_to_string(dir.join("mod.rs")).expect("vault/mod.rs");
    let parsed = syn::parse_file(&mod_rs).expect("vault/mod.rs parses");
    let test_only: HashSet<String> = parsed
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Mod(m) if m.content.is_none() && cfg_implies_test(&m.attrs) => {
                Some(format!("{}.rs", m.ident))
            }
            _ => None,
        })
        .collect();
    rust_files(dir)
        .into_iter()
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !test_only.contains(name)
        })
        .collect()
}

#[test]
fn the_vault_compares_secrets_in_constant_time_and_zeroes_argon2_memory() {
    let dir = crate_dir("apprafter-core").join("src").join("vault");
    let files = vault_production_files(&dir);
    assert!(
        files.iter().any(|p| p.ends_with("seal.rs")),
        "no seal.rs among {files:?}"
    );
    let mut findings = Vec::new();
    let mut ct_eq = 0;
    for path in &files {
        let rel = path.strip_prefix(cli_dir()).unwrap().display().to_string();
        let (hits, count) = vault_rules_source(&rel, &fs::read_to_string(path).unwrap());
        findings.extend(hits);
        ct_eq += count;
    }
    assert!(
        findings.is_empty(),
        "the vault's code rules (D.4a T17, T29):\n{}",
        findings.join("\n")
    );
    assert!(
        ct_eq >= VAULT_CT_EQ_MIN,
        "the vault makes {ct_eq} constant-time comparisons, at least {VAULT_CT_EQ_MIN} expected: \
         did one become `==`?"
    );
}

mod vault_rules {
    use super::*;

    fn rules(src: &str) -> (Vec<String>, usize) {
        vault_rules_source("vault/x.rs", src)
    }

    #[test]
    fn a_secret_compared_with_eq_is_found_and_ct_eq_is_counted() {
        let (hits, ct_eq) = rules(
            "\
fn a(keys: &SlotKeys, stored: &[u8; 32]) -> bool { keys.key_check[..] == stored[..] }
fn b(dk: &DataKey, other: &[u8; 32]) -> bool { dk.expose() != other }
fn c(keys: &SlotKeys, stored: &[u8; 32]) -> bool { bool::from(keys.key_check[..].ct_eq(&stored[..])) }
fn d(n: usize) -> bool { n == 0 }
",
        );
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert!(hits[0].starts_with("vault/x.rs:1:"), "{hits:?}");
        assert!(hits[1].starts_with("vault/x.rs:2:"), "{hits:?}");
        assert_eq!(ct_eq, 1);
    }

    #[test]
    fn argon2s_allocating_calls_and_a_serialize_derive_are_found() {
        let (hits, _) = rules(
            "\
fn f(argon: &Argon2) { argon.hash_password_into(b\"p\", b\"saltsalt\", &mut [0; 32]).unwrap(); }
#[derive(Debug, serde::Serialize)]
struct S;
",
        );
        assert_eq!(hits.len(), 2, "{hits:?}");
    }

    #[test]
    fn test_code_is_not_held_to_them() {
        let (hits, ct_eq) = rules(
            "\
#[cfg(test)]
mod tests {
    fn t(kek: [u8; 32]) { assert!(kek == [0; 32]); let _ = kek[..].ct_eq(&[0; 32]); }
}
",
        );
        assert!(hits.is_empty(), "{hits:?}");
        assert_eq!(ct_eq, 0);
    }
}

// ---------------------------------------------------------------------
// G-OS2 (D.4b B43): the os-slots stand-ins stay in tests
// ---------------------------------------------------------------------

#[test]
fn the_os_slots_stand_ins_reach_no_shipped_build() {
    let manifests = every_manifest();
    let hits: Vec<String> = manifests
        .iter()
        .flat_map(|(rel, text)| feature_edges(&OS_SLOTS_TEST_SUPPORT, rel, text))
        .collect();
    assert!(
        hits.is_empty(),
        "the os-slots stand-ins must never reach a shipped build (D.4b B43, G-OS2):\n{}",
        hits.join("\n")
    );
    let (_, own) = manifests
        .iter()
        .find(|(rel, _)| rel == "cli/os-slots/Cargo.toml")
        .expect("cli/os-slots/Cargo.toml is there");
    let doc: toml::Value = own.parse().unwrap();
    assert!(
        doc.get("features")
            .and_then(|f| f.get(OS_SLOTS_TEST_SUPPORT.feature))
            .is_some(),
        "apprafter-os-slots declares no `test-support`: this guard would see nothing"
    );
    for (file, feature) in OS_SLOTS_TEST_SUPPORT.forwarders {
        let (_, text) = manifests
            .iter()
            .find(|(rel, _)| rel == file)
            .unwrap_or_else(|| panic!("{file} is gone: a stale forwarder"));
        let doc: toml::Value = text.parse().unwrap();
        assert!(
            doc.get("features").and_then(|f| f.get(*feature)).is_some(),
            "{file} has no feature `{feature}`: a stale forwarder"
        );
    }
}

mod os_slots_test_support {
    use super::*;

    fn edges(file: &str, manifest: &str) -> usize {
        feature_edges(&OS_SLOTS_TEST_SUPPORT, file, manifest).len()
    }

    #[test]
    fn a_dev_dependency_may_enable_it_and_nothing_else_may() {
        let dev = "[dev-dependencies]\napprafter-os-slots = { path = \"x\", features = [\"test-support\"] }\n";
        assert_eq!(edges("cli/x/Cargo.toml", dev), 0);
        for m in [
            "[dependencies]\napprafter-os-slots = { path = \"x\", features = [\"test-support\"] }\n",
            "[target.'cfg(unix)'.dependencies]\nslots = { package = \"apprafter-os-slots\", path = \"x\", features = [\"test-support\"] }\n",
            "[features]\nwalk = [\"apprafter-os-slots/test-support\"]\n",
            "[package]\nname = \"apprafter-os-slots\"\n[features]\ntest-support = []\ndefault = [\"test-support\"]\n",
        ] {
            assert_eq!(edges("cli/x/Cargo.toml", m), 1, "{m}");
        }
    }
}

// ---------------------------------------------------------------------
// G-OS4 (D.4b B17), its crate rule: what reaches PAM, the Keychain or LocalAuthentication
// ---------------------------------------------------------------------

/// Crates that reach the OS's authentication APIs, and the manifests allowed to depend on
/// them: D.2's PAM (`nonstick`) and LocalAuthentication stay in `desktop/os-auth`;
/// `apprafter-os-slots` loads libpam by `dlopen` through `libc` and needs none of them.
const OS_AUTH_CRATES: &[(&str, &[&str])] = &[
    ("nonstick", &["desktop/os-auth/Cargo.toml"]),
    ("libpam-sys", &["desktop/os-auth/Cargo.toml"]),
    (
        "objc2-local-authentication",
        &["desktop/os-auth/Cargo.toml"],
    ),
    ("pam", &[]),
    ("pam-sys", &[]),
    ("pam-client", &[]),
    ("libloading", &[]),
    ("security-framework", &[]),
    ("keyring", &[]),
    ("objc2-security", &[]),
];

/// A manifest's dependencies on [`OS_AUTH_CRATES`] it may not have, in any table, renamed or
/// not.
fn os_auth_dependencies(file: &str, manifest: &str) -> Vec<String> {
    let doc: toml::Value = manifest
        .parse()
        .unwrap_or_else(|e| panic!("{file} does not parse: {e}"));
    let mut hits = Vec::new();
    for (place, table) in dependency_tables(&doc, true) {
        for (name, spec) in table {
            let package = spec.get("package").and_then(|p| p.as_str()).unwrap_or(name);
            let rule = OS_AUTH_CRATES.iter().find(|(krate, _)| *krate == package);
            if let Some((krate, allowed)) = rule {
                if !allowed.contains(&file) {
                    hits.push(format!("{file}: [{place}] depends on `{krate}`"));
                }
            }
        }
    }
    hits
}

#[test]
fn os_authentication_crates_stay_in_the_os_crates() {
    let manifests = every_manifest();
    let hits: Vec<String> = manifests
        .iter()
        .flat_map(|(rel, text)| os_auth_dependencies(rel, text))
        .collect();
    assert!(
        hits.is_empty(),
        "a crate that reaches PAM, the Keychain or LocalAuthentication outside desktop/os-auth \
         (D.4b B17, G-OS4; the CLI loads libpam by dlopen):\n{}",
        hits.join("\n")
    );
    let (_, os_auth) = manifests
        .iter()
        .find(|(rel, _)| rel == "desktop/os-auth/Cargo.toml")
        .expect("desktop/os-auth/Cargo.toml is there");
    assert!(
        !os_auth_dependencies("cli/x/Cargo.toml", os_auth).is_empty(),
        "desktop/os-auth depends on none of OS_AUTH_CRATES any more: this guard tells nothing apart"
    );
}

mod os_auth_crates {
    use super::*;

    #[test]
    fn a_dependency_on_an_os_auth_crate_is_seen_in_every_table() {
        for m in [
            "[dependencies]\nnonstick = \"0.1\"\n",
            "[dev-dependencies]\nnonstick = \"0.1\"\n",
            "[build-dependencies]\nlibloading = \"0.8\"\n",
            "[target.'cfg(unix)'.dependencies]\nn = { package = \"nonstick\", version = \"0.1\" }\n",
            "[workspace.dependencies]\nsecurity-framework = \"3\"\n",
        ] {
            assert_eq!(os_auth_dependencies("cli/x/Cargo.toml", m).len(), 1, "{m}");
        }
        let allowed = "[dependencies]\nnonstick = \"0.1\"\n";
        assert!(os_auth_dependencies("desktop/os-auth/Cargo.toml", allowed).is_empty());
        let other = "[dependencies]\nnonstickier = \"1\"\n";
        assert!(os_auth_dependencies("cli/x/Cargo.toml", other).is_empty());
    }
}

// ---------------------------------------------------------------------
// G-OS1, G-OS4 (symbols), G-OS5, G-OS6 (D.4b B16, B17, B43, B82): real OS backends stay fenced
// ---------------------------------------------------------------------

/// The desktop workspace's crates, read beside [`CLI_MEMBERS`].
const DESKTOP_MEMBERS: &[&str] = &["src-tauri", "ipc", "os-auth"];

/// The fence: `admit` written bare there is the fence's own.
const FENCE_FILE: &str = "cli/os-slots/src/fence.rs";

/// G-OS1: what is, or makes, a real OS backend — the type names, and the paths (by their last
/// segments) of the fence and of the system's PAM. A test may name none of them outside
/// [`REAL_BACKEND_TESTS`] and [`REAL_BACKEND_SELF_TESTS`]. `SystemHello` is d4b-w1's.
const REAL_BACKEND_NAMES: &[&str] = &["SystemAccountPassword", "SystemHello", "SystemLibrary"];
/// `system_library` is the system's libpam without the name `SystemLibrary` (critic round).
const REAL_BACKEND_CALLS: &[&[&str]] =
    &[&["fence", "admit"], &["Pam", "system"], &["system_library"]];

/// G-OS1: the real-backend tests. Every `#[test]` in them is `#[ignore = "…"]` — the script
/// named there runs it in a container or a CI job, against an account it made — and every
/// call of `fence::admit` there passes `Some(…)` roots, so the marker rule holds (B16).
const REAL_BACKEND_TESTS: &[&str] = &[
    "cli/os-slots/tests/pam_container.rs",
    "cli/os-slots/tests/macos_real.rs",
];

/// G-OS1: tests that name a real backend and still reach no PAM — `admit` with a scratch root
/// (refused before anything is made), the real backend built over a scripted libpam (its
/// clock), and the loader over paths that do not exist.
const REAL_BACKEND_SELF_TESTS: &[(&str, &str)] = &[
    (
        "cli/os-slots/src/fence.rs",
        "admit_fences_the_running_account_outside_scratch_roots",
    ),
    (
        "cli/os-slots/src/fence.rs",
        "the_real_backend_reads_its_own_clock",
    ),
    (
        "cli/os-slots/src/pam.rs",
        "a_library_that_does_not_load_names_every_path_tried",
    ),
];

/// G-OS1: the production functions that call `fence::admit` and hand its backend on, as `(file,
/// function)`. Every production caller outside the fence must be listed — d4b-e1a's and
/// d4b-g1b's glue add theirs — and a test that calls a listed one (by its last path segment) is
/// held to G-OS1 as if it called `admit`, so a wrapper cannot hide the real backend (critic
/// round). A caller of a wrapper is not followed (Risk 6).
const ADMIT_WRAPPERS: &[(&str, &str)] = &[];

/// G-OS5: one thing that builds a real backend in production code, and the one `(file,
/// function)` that may: a one-segment `path` is a name any path through which counts (the
/// backend's type — a struct literal, `Self` included — or `system_library`), a longer one a
/// call's path. A struct, not a tuple: clippy's `type_complexity` refuses the tuple slice
/// (critic round).
struct Maker {
    path: &'static [&'static str],
    file: &'static str,
    function: &'static str,
}

/// G-OS5's makers. d4b-w1 adds the Hello backend.
const REAL_BACKEND_MAKERS: &[Maker] = &[
    Maker {
        path: &["SystemAccountPassword"],
        file: "cli/os-slots/src/fence.rs",
        function: "admit",
    },
    Maker {
        path: &["Pam", "system"],
        file: "cli/os-slots/src/fence.rs",
        function: "admit",
    },
    Maker {
        path: &["SystemLibrary", "new"],
        file: "cli/os-slots/src/pam.rs",
        function: "system_library",
    },
    Maker {
        path: &["system_library"],
        file: "cli/os-slots/src/pam.rs",
        function: "system",
    },
];

/// G-OS4: an OS authentication API symbol and the directories that may use it. A `prefix`
/// entry matches the name followed by one or more `[A-Za-z0-9_]`; an `ffi` entry matches only
/// as a symbol — an `extern` item, a C string, a byte string ending in a NUL, or a string
/// literal that is exactly the symbol — so D.2's `pam_in` identifier is no hit. A struct, not a
/// tuple, for clippy's `type_complexity` (critic round).
struct OsSymbol {
    name: &'static str,
    prefix: bool,
    ffi: bool,
    allowed: &'static [&'static str],
}

const OS_SYMBOLS: &[OsSymbol] = &[
    OsSymbol {
        name: "pam_",
        prefix: true,
        ffi: true,
        allowed: &["cli/os-slots/", "desktop/os-auth/"],
    },
    OsSymbol {
        name: "KeyCredentialManager",
        prefix: false,
        ffi: false,
        allowed: &["cli/os-slots/", "desktop/os-auth/"],
    },
    OsSymbol {
        name: "NCrypt",
        prefix: true,
        ffi: false,
        allowed: &["cli/os-slots/", "desktop/os-auth/"],
    },
    OsSymbol {
        name: "SecItem",
        prefix: true,
        ffi: false,
        allowed: &["desktop/os-auth/"],
    },
    OsSymbol {
        name: "SecKeychain",
        prefix: true,
        ffi: false,
        allowed: &["desktop/os-auth/"],
    },
    OsSymbol {
        name: "LAContext",
        prefix: false,
        ffi: false,
        allowed: &["desktop/os-auth/"],
    },
];

/// G-OS6: the files that may build `NewSlot::OsPassword` in production code — the os-slots
/// glue of the CLI (d4b-e1a) and of the desktop (d4b-g1b), which alone ran the check that makes
/// the slot's text an account password; the core cannot check it.
const OS_SLOT_GLUE: &[&str] = &[
    "cli/platform-cli/src/os_slots.rs",
    "desktop/src-tauri/src/os_slots.rs",
];

/// How a name appears, for [`OS_SYMBOLS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolForm {
    /// An identifier anywhere: matches the non-`ffi` entries.
    Ident,
    /// An `extern` item, a C string, a NUL-terminated byte string: matches every entry.
    Symbol,
    /// A plain string, or a byte string without a NUL: matches the `ffi` entries only.
    Text,
}

/// The directories allowed for the [`OS_SYMBOLS`] entry `name` matches in `form`, if any.
fn os_symbol(name: &str, form: SymbolForm) -> Option<&'static [&'static str]> {
    OS_SYMBOLS.iter().find_map(|symbol| {
        let applies = match form {
            SymbolForm::Ident => !symbol.ffi,
            SymbolForm::Symbol => true,
            SymbolForm::Text => symbol.ffi,
        };
        let matches = if symbol.prefix {
            name.strip_prefix(symbol.name).is_some_and(|rest| {
                !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        } else {
            name == symbol.name
        };
        (applies && matches).then_some(symbol.allowed)
    })
}

/// One Rust file G-OS reads: its path from the repository root, its text, and whether all of it
/// is test code (under `tests/`, or an out-of-line `#[cfg(test)]` module).
struct GuardedFile {
    rel: String,
    src: String,
    whole_test: bool,
}

/// Every Rust file of the `cli/` workspace's members and of the desktop's three crates.
fn guarded_files() -> Vec<GuardedFile> {
    let repo = cli_dir()
        .parent()
        .expect("cli/ sits in the repository")
        .to_path_buf();
    let crates = CLI_MEMBERS
        .iter()
        .map(|member| cli_dir().join(member))
        .chain(
            DESKTOP_MEMBERS
                .iter()
                .map(|member| repo.join("desktop").join(member)),
        );
    let mut out = Vec::new();
    for krate in crates {
        let src_files = rust_files(&krate.join("src"));
        let (mut test_files, mut test_dirs) = (Vec::new(), Vec::new());
        for file in &src_files {
            let scan = scan_source(file, &fs::read_to_string(file).unwrap());
            test_files.extend(scan.test_module_files);
            test_dirs.extend(scan.test_module_dirs);
        }
        let mut files: Vec<(PathBuf, bool)> = src_files
            .into_iter()
            .map(|file| {
                let whole =
                    test_files.contains(&file) || test_dirs.iter().any(|d| file.starts_with(d));
                (file, whole)
            })
            .collect();
        let tests_dir = krate.join("tests");
        if tests_dir.is_dir() {
            files.extend(rust_files(&tests_dir).into_iter().map(|file| (file, true)));
        }
        let build = krate.join("build.rs");
        if build.is_file() {
            files.push((build, false));
        }
        for (path, whole_test) in files {
            let rel = path
                .strip_prefix(&repo)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
            out.push(GuardedFile {
                rel,
                src,
                whole_test,
            });
        }
    }
    assert!(
        out.iter().any(|f| f.rel.starts_with("cli/os-slots/src/"))
            && out
                .iter()
                .any(|f| f.rel.starts_with("desktop/os-auth/src/")),
        "G-OS reads neither the os-slots sources nor the desktop's"
    );
    out
}

/// What G-OS finds in one file.
#[derive(Debug, Default)]
struct OsScan {
    /// G-OS1: a real backend named or made in test code: `(line, enclosing function, what)`.
    test_uses: Vec<(usize, String, String)>,
    /// G-OS1: every `#[test]` function, and whether it carries `#[ignore = "…"]`.
    tests: Vec<(String, bool)>,
    /// G-OS1: every call of `fence::admit`, and whether its first argument is `Some(…)`.
    admits: Vec<(usize, bool)>,
    /// G-OS1: `fence::admit` called in production code: `(line, enclosing function)`.
    production_admits: Vec<(usize, String)>,
    /// G-OS4: an OS API symbol: `(line, name, form, the directories allowed)`.
    symbols: Vec<(usize, String, &'static str, &'static [&'static str])>,
    /// G-OS5: a real backend built in production code: `(line, enclosing function, rule)`.
    makers: Vec<(usize, String, usize)>,
    /// G-OS5: `Default` derived or implemented for a real backend: `(line, type)`.
    defaults: Vec<(usize, String)>,
    /// G-OS6: `NewSlot::OsPassword { … }` built in production code: its lines.
    os_password_slots: Vec<usize>,
}

struct OsVisitor {
    whole_test: bool,
    /// `cfg(test)` items and `#[test]` functions entered.
    test_depth: usize,
    fns: Vec<String>,
    impl_types: Vec<Option<String>>,
    /// Local names for `fence::admit` (`use …::fence::admit as a;`), and whether `fence::*` is
    /// imported.
    admit_names: HashSet<String>,
    fence_glob: bool,
    /// Local names for `NewSlot::OsPassword`, and whether `NewSlot::*` is imported.
    os_password_names: HashSet<String>,
    newslot_glob: bool,
    /// The function names of [`ADMIT_WRAPPERS`] (a fixture passes its own).
    wrappers: Vec<String>,
    scan: OsScan,
}

/// Whether `segments` ends with `tail`.
fn ends_with(segments: &[String], tail: &[&str]) -> bool {
    segments.len() >= tail.len()
        && segments[segments.len() - tail.len()..]
            .iter()
            .zip(tail)
            .all(|(a, b)| a == b)
}

/// Every `use` leaf of a file, at any depth: (the full path, its local name, `*` for a glob).
fn use_leaves(file: &syn::File) -> Vec<(Vec<String>, String)> {
    fn walk(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<(Vec<String>, String)>) {
        match tree {
            UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                walk(&p.tree, prefix, out);
                prefix.pop();
            }
            UseTree::Name(n) if n.ident == "self" => {
                out.push((prefix.clone(), prefix.last().cloned().unwrap_or_default()));
            }
            UseTree::Name(n) => {
                let mut full = prefix.clone();
                full.push(n.ident.to_string());
                out.push((full, n.ident.to_string()));
            }
            UseTree::Rename(r) => {
                let mut full = prefix.clone();
                full.push(r.ident.to_string());
                out.push((full, r.rename.to_string()));
            }
            UseTree::Glob(_) => out.push((prefix.clone(), "*".to_owned())),
            UseTree::Group(g) => {
                for item in &g.items {
                    walk(item, prefix, out);
                }
            }
        }
    }
    struct Uses(Vec<(Vec<String>, String)>);
    impl<'ast> Visit<'ast> for Uses {
        fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
            walk(&u.tree, &mut Vec::new(), &mut self.0);
        }
    }
    let mut uses = Uses(Vec::new());
    uses.visit_file(file);
    uses.0
}

/// G-OS's reading of one file, with [`ADMIT_WRAPPERS`].
fn os_scan(rel: &str, src: &str, whole_test: bool) -> OsScan {
    let wrappers: Vec<&str> = ADMIT_WRAPPERS
        .iter()
        .map(|&(_, function)| function)
        .collect();
    os_scan_with(rel, src, whole_test, &wrappers)
}

/// G-OS's reading of one file, `wrappers` naming the production callers of `admit`.
fn os_scan_with(rel: &str, src: &str, whole_test: bool, wrappers: &[&str]) -> OsScan {
    let file = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {rel}: {e}"));
    let mut v = OsVisitor {
        whole_test: whole_test || cfg_implies_test(&file.attrs),
        test_depth: 0,
        fns: Vec::new(),
        impl_types: Vec::new(),
        admit_names: HashSet::new(),
        fence_glob: false,
        os_password_names: HashSet::new(),
        newslot_glob: false,
        wrappers: wrappers.iter().map(|w| (*w).to_owned()).collect(),
        scan: OsScan::default(),
    };
    if rel == FENCE_FILE {
        v.admit_names.insert("admit".to_owned());
    }
    for (full, local) in use_leaves(&file) {
        let last = full.last().map(String::as_str);
        if local == "*" {
            v.fence_glob |= last == Some("fence");
            v.newslot_glob |= last == Some("NewSlot");
        } else if ends_with(&full, &["fence", "admit"]) {
            v.admit_names.insert(local);
        } else if ends_with(&full, &["NewSlot", "OsPassword"]) {
            v.os_password_names.insert(local);
        }
    }
    v.visit_file(&file);
    v.scan
}

impl OsVisitor {
    fn in_test(&self) -> bool {
        self.whole_test || self.test_depth > 0
    }

    fn site(&self) -> String {
        self.fns.last().cloned().unwrap_or_default()
    }

    /// A path's segments, `Self` read as the type of the impl it is in.
    fn segments(&self, path: &syn::Path) -> Vec<String> {
        path.segments
            .iter()
            .map(|s| {
                let name = s.ident.to_string();
                match self.impl_types.last() {
                    Some(Some(ty)) if name == "Self" => ty.clone(),
                    _ => name,
                }
            })
            .collect()
    }

    fn is_admit(&self, segments: &[String]) -> bool {
        ends_with(segments, &["fence", "admit"])
            || matches!(segments, [one] if self.admit_names.contains(one) || (self.fence_glob && one == "admit"))
    }

    fn is_os_password(&self, segments: &[String]) -> bool {
        ends_with(segments, &["NewSlot", "OsPassword"])
            || matches!(segments, [one] if self.os_password_names.contains(one) || (self.newslot_glob && one == "OsPassword"))
    }

    /// G-OS1 in test code, G-OS5 in production code, for a path in an expression.
    fn expr_path(&mut self, segments: &[String], line: usize) {
        if self.in_test() {
            let named = segments
                .iter()
                .any(|s| REAL_BACKEND_NAMES.contains(&s.as_str()));
            let called = self.is_admit(segments)
                || REAL_BACKEND_CALLS
                    .iter()
                    .any(|tail| ends_with(segments, tail))
                || segments
                    .last()
                    .is_some_and(|last| self.wrappers.contains(last));
            if named || called {
                self.scan
                    .test_uses
                    .push((line, self.site(), segments.join("::")));
            }
        } else {
            for (rule, maker) in REAL_BACKEND_MAKERS.iter().enumerate() {
                let hit = match maker.path {
                    [name] => segments.iter().any(|s| s == name),
                    tail => ends_with(segments, tail),
                };
                if hit {
                    self.scan.makers.push((line, self.site(), rule));
                }
            }
        }
    }

    fn symbol(&mut self, name: &str, form: SymbolForm, label: &'static str, line: usize) {
        if let Some(allowed) = os_symbol(name, form) {
            self.scan
                .symbols
                .push((line, name.to_owned(), label, allowed));
        }
    }

    /// A literal, for G-OS4 (a macro's tokens hand theirs over with their own line).
    fn literal(&mut self, lit: &syn::Lit, line: usize) {
        match lit {
            syn::Lit::CStr(c) => {
                self.symbol(
                    &c.value().to_string_lossy(),
                    SymbolForm::Symbol,
                    "C string",
                    line,
                );
            }
            syn::Lit::ByteStr(b) => {
                let bytes = b.value();
                match bytes.strip_suffix(b"\0") {
                    Some(name) => {
                        self.symbol(
                            &String::from_utf8_lossy(name),
                            SymbolForm::Symbol,
                            "byte string",
                            line,
                        );
                    }
                    None => {
                        self.symbol(
                            &String::from_utf8_lossy(&bytes),
                            SymbolForm::Text,
                            "byte string",
                            line,
                        );
                    }
                }
            }
            syn::Lit::Str(s) => self.symbol(&s.value(), SymbolForm::Text, "string", line),
            _ => {}
        }
    }

    /// A macro body that parses neither as expressions nor as statements: its tokens.
    fn macro_tokens(&mut self, tokens: TokenStream) {
        let mut previous: Option<String> = None;
        for tree in tokens {
            match tree {
                TokenTree::Group(g) => self.macro_tokens(g.stream()),
                TokenTree::Ident(i) => {
                    let name = i.to_string();
                    let line = line_of(i.span());
                    self.symbol(&name, SymbolForm::Ident, "identifier", line);
                    let admit = name == "admit" && previous.as_deref() == Some("fence");
                    let call = self.wrappers.contains(&name)
                        || REAL_BACKEND_CALLS
                            .iter()
                            .any(|tail| *tail == [name.as_str()]);
                    let named = REAL_BACKEND_NAMES.contains(&name.as_str());
                    if self.in_test() && (admit || call || named) {
                        self.scan.test_uses.push((line, self.site(), name.clone()));
                    }
                    previous = Some(name);
                }
                TokenTree::Literal(l) => {
                    if let Ok(lit) = syn::parse_str::<syn::Lit>(&l.to_string()) {
                        self.literal(&lit, line_of(l.span()));
                    }
                }
                TokenTree::Punct(_) => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for OsVisitor {
    fn visit_item(&mut self, item: &'ast Item) {
        let test = usize::from(cfg_implies_test(item_attrs(item)));
        self.test_depth += test;
        visit::visit_item(self, item);
        self.test_depth -= test;
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        let test = usize::from(cfg_implies_test(impl_item_attrs(item)));
        self.test_depth += test;
        visit::visit_impl_item(self, item);
        self.test_depth -= test;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let is_test = f
            .attrs
            .iter()
            .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"));
        if is_test {
            let ignored = f
                .attrs
                .iter()
                .any(|a| a.path().is_ident("ignore") && matches!(a.meta, Meta::NameValue(_)));
            self.scan.tests.push((f.sig.ident.to_string(), ignored));
        }
        self.test_depth += usize::from(is_test);
        self.fns.push(f.sig.ident.to_string());
        visit::visit_item_fn(self, f);
        self.fns.pop();
        self.test_depth -= usize::from(is_test);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.fns.push(f.sig.ident.to_string());
        visit::visit_impl_item_fn(self, f);
        self.fns.pop();
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let ty = match &*i.self_ty {
            syn::Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        let default = i
            .trait_
            .as_ref()
            .is_some_and(|(_, path, _)| path.segments.last().is_some_and(|s| s.ident == "Default"));
        if let Some(ty) = ty
            .as_ref()
            .filter(|ty| default && REAL_BACKEND_NAMES.contains(&ty.as_str()))
        {
            self.scan
                .defaults
                .push((line_of(i.impl_token.span), ty.clone()));
        }
        self.impl_types.push(ty);
        visit::visit_item_impl(self, i);
        self.impl_types.pop();
    }

    fn visit_item_struct(&mut self, s: &'ast syn::ItemStruct) {
        let name = s.ident.to_string();
        let derives_default = s.attrs.iter().any(|a| {
            a.path().is_ident("derive")
                && a.parse_args_with(Punctuated::<syn::Path, syn::Token![,]>::parse_terminated)
                    .is_ok_and(|list| {
                        list.iter()
                            .any(|p| p.segments.last().is_some_and(|seg| seg.ident == "Default"))
                    })
        });
        if derives_default && REAL_BACKEND_NAMES.contains(&name.as_str()) {
            self.scan.defaults.push((line_of(s.ident.span()), name));
        }
        visit::visit_item_struct(self, s);
    }

    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let Expr::Path(p) = &*c.func {
            let segments = self.segments(&p.path);
            if self.is_admit(&segments) {
                let some = matches!(
                    c.args.first(),
                    Some(Expr::Call(inner)) if matches!(&*inner.func, Expr::Path(q) if q.path.is_ident("Some"))
                );
                let line = line_of(p.path.segments[0].ident.span());
                self.scan.admits.push((line, some));
                if !self.in_test() {
                    self.scan.production_admits.push((line, self.site()));
                }
            }
        }
        visit::visit_expr_call(self, c);
    }

    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        let segments = self.segments(&p.path);
        self.expr_path(&segments, line_of(p.path.segments[0].ident.span()));
        visit::visit_expr_path(self, p);
    }

    fn visit_expr_struct(&mut self, s: &'ast syn::ExprStruct) {
        let segments = self.segments(&s.path);
        let line = line_of(s.path.segments[0].ident.span());
        self.expr_path(&segments, line);
        if !self.in_test() && self.is_os_password(&segments) {
            self.scan.os_password_slots.push(line);
        }
        visit::visit_expr_struct(self, s);
    }

    fn visit_type_path(&mut self, t: &'ast syn::TypePath) {
        if self.in_test() {
            let named = t
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .find(|n| REAL_BACKEND_NAMES.contains(&n.as_str()));
            if let Some(name) = named {
                let line = line_of(t.path.segments[0].ident.span());
                self.scan.test_uses.push((line, self.site(), name));
            }
        }
        visit::visit_type_path(self, t);
    }

    fn visit_ident(&mut self, i: &'ast proc_macro2::Ident) {
        self.symbol(
            &i.to_string(),
            SymbolForm::Ident,
            "identifier",
            line_of(i.span()),
        );
    }

    fn visit_lit(&mut self, l: &'ast syn::Lit) {
        self.literal(l, line_of(l.span()));
        visit::visit_lit(self, l);
    }

    fn visit_foreign_item_fn(&mut self, f: &'ast syn::ForeignItemFn) {
        let line = line_of(f.sig.ident.span());
        self.symbol(
            &f.sig.ident.to_string(),
            SymbolForm::Symbol,
            "extern fn",
            line,
        );
        visit::visit_foreign_item_fn(self, f);
    }

    fn visit_foreign_item_static(&mut self, s: &'ast syn::ForeignItemStatic) {
        let line = line_of(s.ident.span());
        self.symbol(
            &s.ident.to_string(),
            SymbolForm::Symbol,
            "extern static",
            line,
        );
        visit::visit_foreign_item_static(self, s);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if let Ok(args) = m.parse_body_with(Punctuated::<Expr, syn::Token![,]>::parse_terminated) {
            for e in &args {
                self.visit_expr(e);
            }
        } else if let Ok(stmts) = m.parse_body_with(syn::Block::parse_within) {
            for s in &stmts {
                self.visit_stmt(s);
            }
        } else {
            self.macro_tokens(m.tokens.clone());
        }
        visit::visit_macro(self, m);
    }
}

#[test]
fn no_test_reaches_a_real_os_backend() {
    let files = guarded_files();
    let mut problems = Vec::new();
    for f in &files {
        let scan = os_scan(&f.rel, &f.src, f.whole_test);
        for (line, site) in &scan.production_admits {
            let listed = ADMIT_WRAPPERS
                .iter()
                .any(|(file, function)| *file == f.rel && function == site);
            if f.rel != FENCE_FILE && !listed {
                problems.push(format!(
                    "{}:{line}: `{site}` calls fence::admit in production code: list it in \
                     ADMIT_WRAPPERS, so G-OS1 sees every test that calls it",
                    f.rel
                ));
            }
        }
        if REAL_BACKEND_TESTS.iter().any(|t| *t == f.rel) {
            assert!(
                !scan.tests.is_empty(),
                "{} holds no test: REAL_BACKEND_TESTS is stale",
                f.rel
            );
            for (name, ignored) in &scan.tests {
                if !ignored {
                    problems.push(format!(
                        "{}: `{name}` is not #[ignore = \"…\"]: it would reach a real backend in every cargo test",
                        f.rel
                    ));
                }
            }
            for (line, some) in &scan.admits {
                if !some {
                    problems.push(format!(
                        "{}:{line}: fence::admit without Some(roots): the real tests pass the fence's marker rule",
                        f.rel
                    ));
                }
            }
            continue;
        }
        for (line, site, what) in &scan.test_uses {
            let allowed = REAL_BACKEND_SELF_TESTS
                .iter()
                .any(|(file, test)| *file == f.rel && test == site);
            if !allowed {
                problems.push(format!(
                    "{}:{line}: `{what}` in test code (`{site}`)",
                    f.rel
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "a test reaches a real PAM or Windows Hello backend (D.4b B43, G-OS1; CLAUDE.md): only the \
         #[ignore]d real tests may, in their container or CI job:\n{}",
        problems.join("\n")
    );
    for file in REAL_BACKEND_TESTS {
        assert!(
            files.iter().any(|f| f.rel == *file),
            "{file} is gone: REAL_BACKEND_TESTS is stale"
        );
    }
    for (file, test) in REAL_BACKEND_SELF_TESTS {
        let f = files
            .iter()
            .find(|f| f.rel == *file)
            .unwrap_or_else(|| panic!("{file} is gone: REAL_BACKEND_SELF_TESTS is stale"));
        let tests = os_scan(&f.rel, &f.src, f.whole_test).tests;
        assert!(
            tests.iter().any(|(n, _)| n == test),
            "{file} has no test `{test}`: stale"
        );
    }
    for (file, function) in ADMIT_WRAPPERS {
        let f = files
            .iter()
            .find(|f| f.rel == *file)
            .unwrap_or_else(|| panic!("{file} is gone: ADMIT_WRAPPERS is stale"));
        let callers = os_scan(&f.rel, &f.src, f.whole_test).production_admits;
        assert!(
            callers.iter().any(|(_, site)| site == function),
            "{file} `{function}` no longer calls fence::admit: ADMIT_WRAPPERS is stale"
        );
    }
}

#[test]
fn only_the_fence_makes_a_real_os_backend() {
    let mut problems = Vec::new();
    let mut made_where_allowed = HashSet::new();
    for f in guarded_files() {
        let scan = os_scan(&f.rel, &f.src, f.whole_test);
        for (line, site, rule) in &scan.makers {
            let maker = &REAL_BACKEND_MAKERS[*rule];
            if f.rel == maker.file && site == maker.function {
                made_where_allowed.insert(*rule);
            } else {
                let what = maker.path.join("::");
                problems.push(format!("{}:{line}: `{what}` in `{site}`", f.rel));
            }
        }
        for (line, name) in &scan.defaults {
            problems.push(format!("{}:{line}: Default for {name}", f.rel));
        }
    }
    assert!(
        problems.is_empty(),
        "a real OS backend built outside the fence (D.4b B16, B82, G-OS5): only fence::admit may:\n{}",
        problems.join("\n")
    );
    for (rule, maker) in REAL_BACKEND_MAKERS.iter().enumerate() {
        assert!(
            made_where_allowed.contains(&rule),
            "{} is no longer built in {} `{}`: REAL_BACKEND_MAKERS is stale",
            maker.path.join("::"),
            maker.file,
            maker.function
        );
    }
}

#[test]
fn os_slots_reads_no_environment_and_never_prints() {
    let findings = scan_tree(&crate_dir("os-slots").join("src"));
    let allowed = |f: &Finding| {
        f.kind == Kind::EnvRead && f.callee.ends_with("confstr") && f.file.ends_with("src/macos.rs")
    };
    let hits: Vec<String> = findings
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                Kind::EnvRead
                    | Kind::EnvWrite
                    | Kind::Dirs
                    | Kind::ProcessEnd
                    | Kind::Terminal
                    | Kind::Print
            )
        })
        .filter(|f| !allowed(f))
        .map(Finding::to_string)
        .collect();
    assert!(
        hits.is_empty(),
        "apprafter-os-slots reads the environment or prints (D.4b B12, B16, B82: the fence and the \
         back-off follow no variable; the caller passes the sandbox roots):\n{}",
        hits.join("\n")
    );
    assert!(
        findings.iter().any(allowed),
        "macos.rs reads no confstr any more: this guard's one exception is stale"
    );
}

#[test]
fn os_api_symbols_stay_in_the_os_crates() {
    let mut problems = Vec::new();
    let mut os_slots_dlsym = false;
    for f in guarded_files() {
        for (line, name, form, allowed) in os_scan(&f.rel, &f.src, f.whole_test).symbols {
            if allowed.iter().any(|dir| f.rel.starts_with(dir)) {
                // Split, not the symbol whole: an exact `pam_*` string here is a G-OS4 hit itself.
                os_slots_dlsym |= f.rel.starts_with("cli/os-slots/")
                    && name.strip_prefix("pam_") == Some("start");
            } else {
                problems.push(format!("{}:{line}: {form} `{name}`", f.rel));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "an OS authentication API outside its crate (D.4b B17, G-OS4): PAM and Windows Hello belong \
         in cli/os-slots and desktop/os-auth, the Keychain and LocalAuthentication in \
         desktop/os-auth only:\n{}",
        problems.join("\n")
    );
    assert!(
        os_slots_dlsym,
        "G-OS4 does not see os-slots' c\"pam_start\": it would see no symbol"
    );
}

#[test]
fn only_the_os_slots_glue_makes_an_account_password_slot() {
    let mut problems = Vec::new();
    for f in guarded_files() {
        if OS_SLOT_GLUE.iter().any(|glue| *glue == f.rel) {
            continue;
        }
        for line in os_scan(&f.rel, &f.src, f.whole_test).os_password_slots {
            problems.push(format!("{}:{line}", f.rel));
        }
    }
    assert!(
        problems.is_empty(),
        "NewSlot::OsPassword built outside the os-slots glue (D.4b B43, G-OS6): only a caller that \
         ran the account-password check may make that slot, and the core cannot check it:\n{}",
        problems.join("\n")
    );
    let seal = fs::read_to_string(crate_dir("apprafter-core").join("src/vault/seal.rs")).unwrap();
    assert!(
        seal.contains("pub enum NewSlot"),
        "vault::NewSlot moved: point G-OS6 at it"
    );
}

mod os_guards {
    use super::*;

    fn scan(rel: &str, src: &str) -> OsScan {
        os_scan(rel, src, false)
    }

    fn sites(scan: &OsScan) -> Vec<String> {
        scan.test_uses
            .iter()
            .map(|(_, site, _)| site.clone())
            .collect()
    }

    #[test]
    fn a_test_that_names_or_makes_a_real_backend_is_seen() {
        let src = "
            #[cfg(test)]
            mod tests {
                #[test]
                fn a() { let _ = apprafter_os_slots::fence::admit(None); }
                #[test]
                fn b() { let _: Option<SystemAccountPassword> = None; }
                #[test]
                fn c() { let _ = crate::pam::Pam::system(Os::Linux); }
                #[test]
                fn d() { assert!(fence::admit(None).is_ok()); }
            }
            fn production() { let _ = apprafter_os_slots::fence::admit(Some(&roots)); }
        ";
        assert_eq!(sites(&scan("cli/x/src/lib.rs", src)), ["a", "b", "c", "d"]);
        let helper = "fn helper() { let _ = fence::admit(None); }";
        assert_eq!(
            os_scan("cli/x/tests/t.rs", helper, true).test_uses.len(),
            1,
            "tests/ is test code"
        );
    }

    #[test]
    fn renamed_glob_and_grouped_imports_of_admit_are_seen() {
        for src in [
            "use apprafter_os_slots::fence::admit as let_in;\n#[test]\nfn t() { let_in(None); }",
            "use apprafter_os_slots::fence::*;\n#[test]\nfn t() { admit(None); }",
            "use apprafter_os_slots::fence::{self, admit};\n#[test]\nfn t() { admit(None); }",
        ] {
            assert_eq!(scan("cli/x/src/lib.rs", src).test_uses.len(), 1, "{src}");
        }
        let other = "#[test]\nfn t() { admit(None); }";
        assert!(
            scan("cli/x/src/lib.rs", other).test_uses.is_empty(),
            "an admit not the fence's"
        );
        assert_eq!(
            scan(FENCE_FILE, other).test_uses.len(),
            1,
            "the fence's own admit"
        );
    }

    #[test]
    fn ignored_tests_and_some_roots_are_read() {
        let src = r#"
            #[test]
            #[ignore = "needs the PAM container"]
            fn real() { let _ = fence::admit(Some(&roots())); }
            #[test]
            #[ignore]
            fn bare_ignore() {}
            #[test]
            fn plain() { let _ = fence::admit(None); }
        "#;
        let s = os_scan("cli/os-slots/tests/pam_container.rs", src, true);
        let tests = [
            ("real".to_owned(), true),
            ("bare_ignore".to_owned(), false),
            ("plain".to_owned(), false),
        ];
        assert_eq!(s.tests, tests);
        let some: Vec<bool> = s.admits.iter().map(|(_, some)| *some).collect();
        assert_eq!(some, [true, false]);
    }

    #[test]
    fn symbols_are_seen_in_every_form_and_identifiers_only_where_they_name_one() {
        let src = r#"
            extern "C" { fn pam_start(); static pam_x: u8; }
            fn f() {
                let _ = c"pam_end";
                let _ = b"pam_get_item\0";
                let _ = "pam_authenticate";
                let _ = KeyCredentialManager::RequestCreateAsync;
                let _ = NCryptOpenStorageProvider;
                let _ = c"SecItemCopyMatching";
                let pam_in = 1;
                let _ = "a pam_start in prose";
                let _ = BCryptVerifySignature;
                println!("{}", LAContext::new());
            }
        "#;
        let found: Vec<String> = scan("cli/x/src/lib.rs", src)
            .symbols
            .into_iter()
            .map(|(_, name, form, _)| format!("{form} {name}"))
            .collect();
        assert_eq!(
            found,
            [
                "extern fn pam_start",
                "extern static pam_x",
                "C string pam_end",
                "byte string pam_get_item",
                "string pam_authenticate",
                "identifier KeyCredentialManager",
                "identifier NCryptOpenStorageProvider",
                "C string SecItemCopyMatching",
                "identifier LAContext",
            ]
        );
    }

    #[test]
    fn backends_built_outside_their_maker_and_default_impls_are_seen() {
        let fence = "
            pub fn admit() -> SystemAccountPassword {
                let os = Os::this();
                SystemAccountPassword { inner: Checker { pam: Pam::system(os) } }
            }
            fn other() -> SystemAccountPassword { SystemAccountPassword { inner: todo!() } }
            impl SystemAccountPassword { fn unfenced() -> Self { Self { inner: todo!() } } }
        ";
        let makers: Vec<(String, usize)> = scan(FENCE_FILE, fence)
            .makers
            .into_iter()
            .map(|(_, site, rule)| (site, rule))
            .collect();
        let expected = [
            ("admit".to_owned(), 0),
            ("admit".to_owned(), 1),
            ("other".to_owned(), 0),
            ("unfenced".to_owned(), 0),
        ];
        assert_eq!(makers, expected);
        let defaults = "#[derive(Debug, Default)]\npub struct SystemAccountPassword { inner: u8 }\nimpl Default for SystemHello { fn default() -> Self { todo!() } }";
        let names: Vec<String> = scan(FENCE_FILE, defaults)
            .defaults
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert_eq!(names, ["SystemAccountPassword", "SystemHello"]);
        let in_test =
            "#[cfg(test)]\nmod t { fn f() { let _ = SystemAccountPassword { inner: 1 }; } }";
        assert!(
            scan(FENCE_FILE, in_test).makers.is_empty(),
            "test code is G-OS1's"
        );
    }

    #[test]
    fn a_production_caller_of_admit_is_seen_and_a_test_calling_a_listed_one_too() {
        let glue = "pub fn fixture_glue_backend(roots: Option<&[PathBuf]>) -> Result<SystemAccountPassword, Fenced> { apprafter_os_slots::fence::admit(roots) }";
        let callers: Vec<String> = scan("cli/x/src/os_slots.rs", glue)
            .production_admits
            .into_iter()
            .map(|(_, site)| site)
            .collect();
        assert_eq!(callers, ["fixture_glue_backend"]);
        let test = "#[test]\nfn t() { let _ = crate::os_slots::fixture_glue_backend(None); }";
        assert!(
            scan("cli/x/src/lib.rs", test).test_uses.is_empty(),
            "an unlisted name is not known"
        );
        let listed = os_scan_with("cli/x/src/lib.rs", test, false, &["fixture_glue_backend"]);
        assert_eq!(
            listed.test_uses.len(),
            1,
            "a test calling a listed wrapper is seen"
        );
        let library = "#[test]\nfn t() { let _ = super::system_library(Os::Linux); }";
        assert_eq!(
            scan("cli/x/src/lib.rs", library).test_uses.len(),
            1,
            "the system's libpam"
        );
    }

    #[test]
    fn an_account_password_slot_built_in_production_is_seen_and_patterns_are_not() {
        for src in [
            "fn f() { let _ = NewSlot::OsPassword { secret, label, enrolled_at, machine }; }",
            "fn f() { let _ = apprafter_core::vault::NewSlot::OsPassword { secret, label, enrolled_at, machine }; }",
            "use apprafter_core::vault::NewSlot::OsPassword;\nfn f() { let _ = OsPassword { secret, label, enrolled_at, machine }; }",
            "use apprafter_core::vault::NewSlot::*;\nfn f() { let _ = OsPassword { secret, label, enrolled_at, machine }; }",
            "impl NewSlot { fn f() -> Self { Self::OsPassword { secret, label, enrolled_at, machine } } }",
        ] {
            assert_eq!(scan("cli/x/src/lib.rs", src).os_password_slots.len(), 1, "{src}");
        }
        for src in [
            "fn f(s: NewSlot) { if let NewSlot::OsPassword { .. } = s {} }",
            "#[cfg(test)]\nmod t { fn f() { let _ = NewSlot::OsPassword { secret, label, enrolled_at, machine }; } }",
            "fn f() { let _ = NewSlot::Password { secret, label }; }",
        ] {
            assert!(scan("cli/x/src/lib.rs", src).os_password_slots.is_empty(), "{src}");
        }
    }
}
