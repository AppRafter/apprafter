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
/// breakdown, with `cargo test -p apprafter-core --test guards -- --nocapture`.
const ENV_READ_BASELINE: usize = 51;

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
    // `HCLOUD_TOKEN` / `HETZNER_SSH_PUBLIC_KEY`, then the target store.
    "cli_core::credentials::resolve_hetzner_token",
    "cli_core::resolve_hetzner_token",
    "cli_core::credentials::resolve_hetzner_ssh_public_key",
    "cli_core::resolve_hetzner_ssh_public_key",
    // `APPRAFTER_CONFIG_DIR`, then `dirs`.
    "cli_core::target::default_config_root",
    "cli_core::default_config_root",
    // `APPRAFTER_AGE_KEY`, then `dirs`.
    "cli_core::secrets::default_age_key_path",
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
];

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

struct Scanner<'f> {
    file: &'f Path,
    scopes: Vec<Scope>,
    /// Inline `mod a { … }` names around the current item.
    inline_mods: Vec<String>,
    /// Self types of the enclosing `impl` blocks, innermost last.
    impl_types: Vec<String>,
    /// `Type::name` / `name` of the enclosing functions, innermost last.
    fns: Vec<String>,
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
    let mut hits = scan_tree(&core.join("src"));
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
use cli_core::resolve_hetzner_token as token;
use cli_providers::k8s::KubectlCli;
fn f() {
    let _ = token();
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
                fc("cli_core::resolve_hetzner_token"),
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
}
