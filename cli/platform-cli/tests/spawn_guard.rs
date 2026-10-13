// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! G1 (D.4a T1, T8; ADR 0068 §9): every test in `cli/` runs the `apprafter` binary, or the
//! `platform-cli` shim, through `tests/common/spawn.rs` — the one place that builds the child's
//! environment from nothing, fences it with `APPRAFTER_SANDBOX` and takes its terminal away.
//!
//! The scan reads tokens (`proc_macro2`), so a comment never counts and a string is one literal;
//! attributes (doc comments included) are skipped; a raw identifier counts as the plain one.
//! Every `.rs` file under `cli/<member>/src` and `cli/<member>/tests` is scanned, except
//! `tests/common/spawn.rs` (the spawn point) and this file (whose rules and fixtures spell what
//! they ban). A file fails on:
//!
//! - the identifier `cargo_bin`, anywhere — `assert_cmd` looks the binary up by a name it takes
//!   at run time, so a guard on the literal `"apprafter"` would miss a computed one — and the
//!   crates that run binaries for tests: `escargot`, `snapbox`, `trycmd`;
//! - a string literal holding the prefix of the variables cargo sets to the binaries it built
//!   ([`BIN_ENV_PREFIX`]), or a path it built one at (`…target/debug/apprafter`,
//!   `…target/release/apprafter`, either separator, `.exe` or not), or the literal program of a
//!   `Command::new` named `apprafter` or `platform-cli` (`clap::Command::new` excepted);
//! - in test code (any file under a `tests/` directory): `current_exe`, and the controls the
//!   spawn point owns — `env_clear`, `pre_exec`, `creation_flags`;
//! - the spawn point's opt-ins — `with_startup_checks`, `sharing_console`, `std_at_terminal`,
//!   `probe_program`, `probe_program_at_terminal`, `in_session` — `without_env` / `env_remove`
//!   of `HOME`, `APPRAFTER_CONFIG_DIR`, `XDG_RUNTIME_DIR` or `APPRAFTER_SANDBOX`, and
//!   `with_env` / `env` of `APPRAFTER_SANDBOX` (the fence dropped or redrawn); the fence's
//!   variable counts whether the call spells it or names the constant `SANDBOX_ENV` (bare or
//!   by path);
//!
//! unless [`ALLOWED`] names the file, the function (or `const`) and the rule, each with its reason.
//! Every entry must name a function that exists in its file
//! ([`every_allowance_names_a_function_that_exists`]). A new opt-out is a new entry.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Literal, Spacing, TokenStream, TokenTree};

/// What a hit breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rule {
    /// `cargo_bin`, or a crate that runs binaries for tests.
    CargoBin,
    /// A string literal with [`BIN_ENV_PREFIX`].
    BinEnv,
    /// A string literal that is a built binary's path, or a `Command::new` of one by name.
    BinPath,
    /// `current_exe` in test code.
    CurrentExe,
    /// `env_clear` in test code.
    EnvClear,
    /// `pre_exec` in test code.
    PreExec,
    /// `creation_flags` in test code.
    CreationFlags,
    /// `Spawn::with_startup_checks`.
    StartupChecks,
    /// `Spawn::sharing_console`.
    SharingConsole,
    /// `Spawn::std_at_terminal` (and `Sandbox::std_at_terminal`).
    AtTerminal,
    /// `Spawn::probe_program`, `Spawn::probe_program_at_terminal`.
    ProbeProgram,
    /// `Spawn::in_session`.
    InSession,
    /// `without_env` / `env_remove` of a variable the fence relies on.
    RemovesFence,
    /// `with_env` / `env` of `APPRAFTER_SANDBOX`: the fence redrawn.
    SetsFence,
}

/// Paths under `cli/` the scan skips (module docs).
const NOT_SCANNED: [&str; 2] = [
    "platform-cli/tests/common/spawn.rs",
    "platform-cli/tests/spawn_guard.rs",
];

/// The prefix of `CARGO_BIN_EXE_<name>`, the variables cargo sets for integration tests.
const BIN_ENV_PREFIX: &str = "CARGO_BIN_EXE_";

/// Crates that build or run binaries for tests.
const RUNNERS: [&str; 3] = ["escargot", "snapbox", "trycmd"];

/// The file names of the binaries this workspace ships.
const BINARY_NAMES: [&str; 4] = [
    "apprafter",
    "apprafter.exe",
    "platform-cli",
    "platform-cli.exe",
];

/// The fence's variable, and the constant the tests name it by (`common::spawn::SANDBOX_ENV`).
const SANDBOX_VAR: &str = "APPRAFTER_SANDBOX";
const SANDBOX_CONST: &str = "SANDBOX_ENV";

/// Variables the fence relies on: without one of the first three, the CLI falls back to a real
/// location; without the fourth there is no fence, and a test can point the others anywhere.
const FENCED_VARS: [&str; 4] = [
    "HOME",
    "APPRAFTER_CONFIG_DIR",
    "XDG_RUNTIME_DIR",
    SANDBOX_VAR,
];

/// Who may break a rule: (file under `cli/`, function or `const`, rule). Each with its reason.
const ALLOWED: &[(&str, &str, Rule)] = &[
    // The Windows tool stand-in: the stand-ins are hard links of it, and the spawn point's
    // Windows self-tests run its probe modes.
    (
        "platform-cli/tests/common/stand_in.rs",
        "stand_in_binary",
        Rule::BinEnv,
    ),
    // Re-runs one golden case in a child of this test binary with a long TMPDIR; the case
    // itself spawns through `Sandbox`.
    (
        "platform-cli/tests/golden.rs",
        "harness_a_long_tmpdir_changes_no_golden",
        Rule::CurrentExe,
    ),
    // The stand-in names itself after the file it was started as.
    (
        "platform-cli/tests/support/apprafter_tool_stand_in.rs",
        "main",
        Rule::CurrentExe,
    ),
    // Mutant 7: re-runs one test of this binary at a pseudo-terminal it makes the controlling
    // terminal (setsid + TIOCSCTTY): the forced condition of the test.
    (
        "platform-cli/tests/spawn_test.rs",
        "the_child_has_no_controlling_terminal",
        Rule::CurrentExe,
    ),
    (
        "platform-cli/tests/spawn_test.rs",
        "the_child_has_no_controlling_terminal",
        Rule::PreExec,
    ),
    // Mutant 8: re-runs one test of this binary at a pseudo-terminal through the spawn point
    // (`probe_program_at_terminal`); a shell would take the terminal on its own (GOTCHA-181).
    (
        "platform-cli/tests/spawn_test.rs",
        "a_terminal_case_owns_its_pty",
        Rule::CurrentExe,
    ),
    // Mutant 9: re-runs one test of this binary with a console of its own (CREATE_NEW_CONSOLE).
    (
        "platform-cli/tests/spawn_test.rs",
        "the_child_has_no_console",
        Rule::CurrentExe,
    ),
    (
        "platform-cli/tests/spawn_test.rs",
        "the_child_has_no_console",
        Rule::CreationFlags,
    ),
    // The spawn point's self-tests: other programs under its rules.
    (
        "platform-cli/tests/spawn_test.rs",
        "child_env",
        Rule::ProbeProgram,
    ),
    (
        "platform-cli/tests/spawn_test.rs",
        "inner_a_spawned_child_cannot_open_dev_tty",
        Rule::ProbeProgram,
    ),
    (
        "platform-cli/tests/spawn_test.rs",
        "a_terminal_case_owns_its_pty",
        Rule::ProbeProgram,
    ),
    (
        "platform-cli/tests/spawn_test.rs",
        "inner_a_spawned_child_has_no_console",
        Rule::ProbeProgram,
    ),
    // Doctor's signal test: the child starts with the signal back at its default disposition
    // (a test run as a background job inherits SIGINT ignored); after the spawn point's setsid.
    (
        "platform-cli/tests/doctor_test.rs",
        "a_signal_stops_doctor_and_kills_the_tool_it_was_probing",
        Rule::PreExec,
    ),
    // Doctor's Windows Ctrl-Break test: a console event needs a shared console.
    (
        "platform-cli/tests/doctor_test.rs",
        "ctrl_break_stops_doctor_and_kills_the_tool_it_was_probing",
        Rule::SharingConsole,
    ),
    // The one test of the startup checks: they must run.
    (
        "platform-cli/tests/startup_checks_test.rs",
        "new",
        Rule::StartupChecks,
    ),
    // The golden harness's pseudo-terminal cases.
    (
        "platform-cli/tests/common/golden.rs",
        "std_at_terminal",
        Rule::AtTerminal,
    ),
    (
        "platform-cli/tests/golden.rs",
        "run_at_a_terminal",
        Rule::AtTerminal,
    ),
    // D.4a d: the conversation harness of the lock's prompt tests.
    (
        "platform-cli/tests/common/terminal.rs",
        "converse_with",
        Rule::AtTerminal,
    ),
    // L5 (D.4a T23): a test build of `lock on` refuses a store outside APPRAFTER_SANDBOX; the
    // case removes the fence to see it, every other variable still in the scratch root.
    (
        "platform-cli/tests/lock_test.rs",
        "a_test_build_locks_no_store_outside_its_sandbox",
        Rule::RemovesFence,
    ),
    // The fence's own rows: a removed HOME, a removed store variable, no fence at all (it
    // changes nothing unless set), and roots the fence must refuse.
    (
        "platform-cli/tests/sandbox_test.rs",
        "a_removed_home_is_refused",
        Rule::RemovesFence,
    ),
    (
        "platform-cli/tests/sandbox_test.rs",
        "an_xdg_config_home_outside_is_refused",
        Rule::RemovesFence,
    ),
    (
        "platform-cli/tests/sandbox_test.rs",
        "without_the_variable_nothing_is_fenced",
        Rule::RemovesFence,
    ),
    (
        "platform-cli/tests/sandbox_test.rs",
        "a_root_that_is_relative_or_missing_is_refused",
        Rule::SetsFence,
    ),
    // The holder's tests run each "terminal" as a session of its own (D.4a T3, T52).
    (
        "platform-cli/tests/holder_unix.rs",
        "session",
        Rule::InSession,
    ),
    (
        "platform-cli/tests/holder_unix.rs",
        "start",
        Rule::InSession,
    ),
    (
        "platform-cli/tests/holder_unix.rs",
        "a_holder_without_an_init_frame_writes_nothing",
        Rule::InSession,
    ),
    // The OS probes of D.4a Task 6: this test binary re-run for an inner probe, one with a console
    // of its own and one detached; they run no apprafter.
    (
        "platform-cli/tests/holder_probe.rs",
        "rerun",
        Rule::CurrentExe,
    ),
    (
        "platform-cli/tests/holder_probe.rs",
        "p5_macos_proc_pidpath_reads_a_process_that_denies_attach",
        Rule::CurrentExe,
    ),
    (
        "platform-cli/tests/holder_probe.rs",
        "p1_windows_a_console_of_its_own_and_a_detached_child_attached_to_it",
        Rule::CreationFlags,
    ),
    (
        "platform-cli/tests/holder_probe.rs",
        "inner_console_owner",
        Rule::CreationFlags,
    ),
    // The Windows holder's tests (D.4a Task 7): a console of their own (CREATE_NEW_CONSOLE) for
    // this test binary re-run as a console's leader and its commands; only `apprafter __holder`
    // runs, keyed by a scratch store.
    (
        "platform-cli/tests/holder_windows.rs",
        "rerun",
        Rule::CurrentExe,
    ),
    (
        "platform-cli/tests/holder_windows.rs",
        "ask",
        Rule::CurrentExe,
    ),
    (
        "platform-cli/tests/holder_windows.rs",
        "inner_leader",
        Rule::CurrentExe,
    ),
    (
        "platform-cli/tests/holder_windows.rs",
        "on_a_console",
        Rule::CreationFlags,
    ),
    (
        "platform-cli/tests/holder_windows.rs",
        "live",
        Rule::CreationFlags,
    ),
    (
        "platform-cli/tests/holder_windows.rs",
        "ask",
        Rule::CreationFlags,
    ),
    (
        "platform-cli/tests/holder_windows.rs",
        "windows_the_holder_keeps_no_handle_of_its_starter",
        Rule::CreationFlags,
    ),
];

/// One rule broken at one line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    line: usize,
    rule: Rule,
    what: String,
}

/// Every hit in `src`, the source of `file` (a path under `cli/`, `/`-separated).
fn hits(file: &str, src: &str) -> Vec<Hit> {
    let tokens: TokenStream = src.parse().unwrap_or_else(|e| panic!("{file}: {e}"));
    let test_code = file.contains("/tests/");
    let mut out = Vec::new();
    walk(file, test_code, None, tokens, &mut out);
    out.sort_by_key(|h| (h.line, h.rule));
    out
}

/// An identifier as rustc reads it: `r#name` is `name`.
fn plain(ident: &proc_macro2::Ident) -> String {
    let name = ident.to_string();
    name.strip_prefix("r#").map(str::to_string).unwrap_or(name)
}

/// The text of a string literal token (`"…"`, `r"…"`, `r#"…"#`), the two escapes a path or a
/// variable name can hold undone; `None` for any other literal.
fn string_value(lit: &Literal) -> Option<String> {
    let raw = lit.to_string();
    if let Some(rest) = raw.strip_prefix('r') {
        let hashes = rest.len() - rest.trim_start_matches('#').len();
        let body = rest.get(hashes..)?.strip_prefix('"')?;
        return Some(body.get(..body.len().checked_sub(1 + hashes)?)?.to_string());
    }
    let body = raw.strip_prefix('"')?.strip_suffix('"')?;
    Some(body.replace("\\\\", "\\").replace("\\\"", "\""))
}

/// `value` is where cargo builds the `apprafter` binary.
fn names_a_built_binary(value: &str) -> bool {
    let value = value.replace('\\', "/");
    let value = value.strip_suffix(".exe").unwrap_or(&value);
    value.ends_with("target/debug/apprafter") || value.ends_with("target/release/apprafter")
}

/// `value`'s file name is one of this workspace's binaries.
fn file_name_is_a_binary(value: &str) -> bool {
    let value = value.replace('\\', "/");
    BINARY_NAMES.contains(&value.rsplit('/').next().unwrap_or(&value))
}

/// The first argument of a `( … )` group: its value when it starts with a string literal, and
/// [`SANDBOX_VAR`] when it ends with the identifier [`SANDBOX_CONST`] (`SANDBOX_ENV`,
/// `&SANDBOX_ENV`, `common::spawn::SANDBOX_ENV`).
fn first_string_arg(group: Option<&TokenTree>) -> Option<String> {
    let Some(TokenTree::Group(g)) = group else {
        return None;
    };
    if g.delimiter() != Delimiter::Parenthesis {
        return None;
    }
    // The tokens before the first top-level comma (a nested group is one token).
    let first: Vec<TokenTree> = g
        .stream()
        .into_iter()
        .take_while(|t| !matches!(t, TokenTree::Punct(p) if p.as_char() == ','))
        .collect();
    match first.first()? {
        TokenTree::Literal(lit) => string_value(lit),
        _ => match first.last()? {
            TokenTree::Ident(id) if plain(id) == SANDBOX_CONST => Some(SANDBOX_VAR.to_string()),
            _ => None,
        },
    }
}

/// `Command ::` right before `tokens[i]`, and not `clap ::` before that.
fn after_command_path(tokens: &[TokenTree], i: usize) -> bool {
    let punct = |at: usize, joint: bool| {
        matches!(&tokens[at], TokenTree::Punct(p)
            if p.as_char() == ':' && (!joint || p.spacing() == Spacing::Joint))
    };
    let ident =
        |at: usize, name: &str| matches!(&tokens[at], TokenTree::Ident(id) if plain(id) == name);
    i >= 3
        && punct(i - 1, false)
        && punct(i - 2, true)
        && ident(i - 3, "Command")
        && !(i >= 6 && punct(i - 4, false) && punct(i - 5, true) && ident(i - 6, "clap"))
}

/// The rule an identifier at `tokens[i]` breaks, if any.
fn ident_rule(
    name: &str,
    test_code: bool,
    tokens: &[TokenTree],
    i: usize,
) -> Option<(Rule, String)> {
    let rule = match name {
        "cargo_bin" => Rule::CargoBin,
        n if RUNNERS.contains(&n) => Rule::CargoBin,
        "current_exe" if test_code => Rule::CurrentExe,
        "env_clear" if test_code => Rule::EnvClear,
        "pre_exec" if test_code => Rule::PreExec,
        "creation_flags" if test_code => Rule::CreationFlags,
        "with_startup_checks" => Rule::StartupChecks,
        "sharing_console" => Rule::SharingConsole,
        "std_at_terminal" => Rule::AtTerminal,
        "probe_program" | "probe_program_at_terminal" => Rule::ProbeProgram,
        "in_session" => Rule::InSession,
        "without_env" | "env_remove" => {
            return first_string_arg(tokens.get(i + 1))
                .filter(|v| FENCED_VARS.contains(&v.as_str()))
                .map(|v| (Rule::RemovesFence, format!("{name}({v:?})")));
        }
        // `std::env::…` and `env!(…)` are no call: the next token is `::` or `!`.
        "with_env" | "env" => {
            return first_string_arg(tokens.get(i + 1))
                .filter(|v| v == SANDBOX_VAR)
                .map(|v| (Rule::SetsFence, format!("{name}({v:?}, …)")));
        }
        "new" if after_command_path(tokens, i) => {
            return first_string_arg(tokens.get(i + 1))
                .filter(|v| file_name_is_a_binary(v))
                .map(|v| (Rule::BinPath, format!("Command::new({v:?})")));
        }
        _ => return None,
    };
    Some((rule, name.to_string()))
}

/// Record `rule` at `line` unless [`ALLOWED`] lets `within` (a function or `const` of `file`)
/// break it.
fn push(
    file: &str,
    within: Option<&str>,
    rule: Rule,
    what: String,
    line: usize,
    out: &mut Vec<Hit>,
) {
    let allowed = within.is_some_and(|item| {
        ALLOWED
            .iter()
            .any(|&(f, name, r)| f == file && name == item && r == rule)
    });
    if !allowed {
        out.push(Hit { line, rule, what });
    }
}

/// Scan `tokens`, inside the function `within` (the nearest `fn` around them, if any).
fn walk(
    file: &str,
    test_code: bool,
    within: Option<&str>,
    tokens: TokenStream,
    out: &mut Vec<Hit>,
) {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    // The function whose body the next brace group is: `fn <name>` seen, its body not yet.
    let mut next_fn: Option<String> = None;
    // A `const` / `static` item being read, until its `;`.
    let mut in_const: Option<String> = None;
    let mut i = 0;
    while i < tokens.len() {
        let here = in_const.as_deref().or(within);
        match &tokens[i] {
            // An attribute, a doc comment included: `#[…]` or `#![…]`.
            TokenTree::Punct(p) if p.as_char() == '#' => {
                i += match (tokens.get(i + 1), tokens.get(i + 2)) {
                    (Some(TokenTree::Group(g)), _) if g.delimiter() == Delimiter::Bracket => 2,
                    (Some(TokenTree::Punct(bang)), Some(TokenTree::Group(g)))
                        if bang.as_char() == '!' && g.delimiter() == Delimiter::Bracket =>
                    {
                        3
                    }
                    _ => 1,
                };
                continue;
            }
            TokenTree::Punct(p) if p.as_char() == ';' => {
                next_fn = None;
                in_const = None;
            }
            TokenTree::Group(g) => {
                if g.delimiter() == Delimiter::Brace {
                    if let Some(name) = next_fn.take() {
                        walk(file, test_code, Some(&name), g.stream(), out);
                        i += 1;
                        continue;
                    }
                }
                walk(file, test_code, here, g.stream(), out);
            }
            TokenTree::Ident(ident) => {
                let name = plain(ident);
                // A declaration: its name is no use of it.
                if name == "fn" {
                    if let Some(TokenTree::Ident(item)) = tokens.get(i + 1) {
                        next_fn = Some(plain(item));
                        i += 2;
                        continue;
                    }
                } else if name == "const" || name == "static" {
                    if let Some(TokenTree::Ident(item)) = tokens.get(i + 1) {
                        let item = plain(item);
                        // `const fn` is a function; `static mut X` names X after `mut`.
                        if item != "fn" && item != "mut" && within.is_none() {
                            in_const = Some(item);
                            i += 2;
                            continue;
                        }
                    }
                }
                if let Some((rule, what)) = ident_rule(&name, test_code, &tokens, i) {
                    push(file, here, rule, what, ident.span().start().line, out);
                }
            }
            TokenTree::Literal(lit) => {
                if let Some(value) = string_value(lit) {
                    let line = lit.span().start().line;
                    if value.contains(BIN_ENV_PREFIX) {
                        push(file, here, Rule::BinEnv, value.clone(), line, out);
                    }
                    if names_a_built_binary(&value) {
                        push(file, here, Rule::BinPath, value, line, out);
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// `cli/`, from this crate's manifest.
fn cli_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("platform-cli sits in cli/")
        .to_path_buf()
}

/// `path` under `cli/`, `/`-separated.
fn rel(path: &Path) -> String {
    path.strip_prefix(cli_dir())
        .expect("a path under cli/")
        .to_string_lossy()
        .replace('\\', "/")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every `.rs` under `src/` and `tests/` of every workspace member (a directory of `cli/` with a
/// `Cargo.toml`).
fn sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(cli_dir()).unwrap() {
        let member = entry.unwrap().path();
        if !member.join("Cargo.toml").is_file() {
            continue;
        }
        for dir in ["src", "tests"] {
            let dir = member.join(dir);
            if dir.is_dir() {
                rust_files(&dir, &mut files);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn every_cli_spawn_goes_through_the_spawn_point() {
    let files = sources();
    let mut found = Vec::new();
    for path in &files {
        let file = rel(path);
        if NOT_SCANNED.contains(&file.as_str()) {
            continue;
        }
        let src = fs::read_to_string(path).unwrap();
        for h in hits(&file, &src) {
            found.push(format!("{file}:{}: {:?} `{}`", h.line, h.rule, h.what));
        }
    }
    assert!(
        found.is_empty(),
        "a test runs the CLI outside tests/common/spawn.rs, or uses one of its opt-ins where \
         tests/spawn_guard.rs does not allow it (D.4a T1): run it through `Spawn`; a new opt-out \
         is a new ALLOWED entry with its reason:\n{}",
        found.join("\n")
    );
    // The scan saw what it guards.
    for file in [
        "platform-cli/tests/common/golden.rs",
        "platform-cli/tests/golden.rs",
        "platform-cli/tests/doctor_test.rs",
        "platform-cli/tests/target_test.rs",
        "platform-cli/src/lib.rs",
        "apprafter-core/tests/guards.rs",
        "cli-core/src/lib.rs",
    ] {
        assert!(files.iter().any(|p| rel(p) == file), "{file} not scanned");
    }
}

#[test]
fn every_allowance_names_a_function_that_exists() {
    for (file, item, rule) in ALLOWED {
        let src = fs::read_to_string(cli_dir().join(file))
            .unwrap_or_else(|e| panic!("ALLOWED names {file}: {e}"));
        assert!(
            src.contains(&format!("fn {item}(")) || src.contains(&format!("const {item}:")),
            "ALLOWED: {file} has no fn {item} (for {rule:?}): a rename left that opt-out unguarded"
        );
    }
}

/// The rules on sources of their own: each way to spawn the CLI is found, and what is none is not.
#[test]
fn the_scan_finds_every_way_to_spawn_the_cli_and_nothing_else() {
    let rules = |file: &str, src: &str| {
        hits(file, src)
            .into_iter()
            .map(|h| h.rule)
            .collect::<Vec<_>>()
    };
    let t = "platform-cli/tests/x.rs";
    let s = "cli-core/src/x.rs";
    for (file, src, want) in [
        // Mutant 1: the old helper.
        (t, "fn cli() -> Command { Command::cargo_bin(\"apprafter\").unwrap() }", vec![Rule::CargoBin]),
        // Mutant 2: renamed, with a computed name, in another crate's tests.
        ("cli-core/tests/x.rs", "use assert_cmd::Command as C; fn f() { C::cargo_bin(&format!(\"app{}\", \"rafter\")); }", vec![Rule::CargoBin]),
        (t, "fn f() { C::r#cargo_bin(\"x\"); }", vec![Rule::CargoBin]),
        (t, "use escargot::CargoBuild;", vec![Rule::CargoBin]),
        // Mutant 3: the variable cargo sets, in a unit test of src.
        (s, "#[cfg(test)] mod t { fn f() { let _ = std::process::Command::new(env!(\"CARGO_BIN_EXE_apprafter\")); } }", vec![Rule::BinEnv]),
        (t, "fn f() { let _ = option_env!(\"CARGO_BIN_EXE_platform-cli\"); }", vec![Rule::BinEnv]),
        (t, "fn f() { let _ = \"../../target/debug/apprafter\"; }", vec![Rule::BinPath]),
        (t, "fn f() { let _ = r\"C:\\w\\target\\release\\apprafter.exe\"; }", vec![Rule::BinPath]),
        (t, "fn f() { std::process::Command::new(\"apprafter\").status(); }", vec![Rule::BinPath]),
        (t, "fn f() { Command::new(\"/opt/bin/platform-cli\"); }", vec![Rule::BinPath]),
        (t, "fn f() { std::env::current_exe(); }", vec![Rule::CurrentExe]),
        (t, "fn f(c: &mut C) { c.env_clear(); c.pre_exec(|| Ok(())); c.creation_flags(8); }", vec![Rule::EnvClear, Rule::PreExec, Rule::CreationFlags]),
        (t, "fn f() { Spawn::new().with_startup_checks(); }", vec![Rule::StartupChecks]),
        (t, "fn f() { Spawn::new().sharing_console(); }", vec![Rule::SharingConsole]),
        (t, "fn f(s: &Spawn) { s.std_at_terminal(&[], fd); s.probe_program(p, &[]); }", vec![Rule::AtTerminal, Rule::ProbeProgram]),
        (t, "fn f(s: Spawn) { s.without_env(\"HOME\"); c.env_remove(\"XDG_RUNTIME_DIR\"); }", vec![Rule::RemovesFence, Rule::RemovesFence]),
        // The fence's own variable, by its literal or by the constant (bare or by path),
        // dropped or redrawn (critic finding 6).
        (t, "fn f(s: Spawn) { s.without_env(\"APPRAFTER_SANDBOX\"); }", vec![Rule::RemovesFence]),
        (t, "fn f(s: Spawn) { s.without_env(SANDBOX_ENV); c.env_remove(common::spawn::SANDBOX_ENV); }", vec![Rule::RemovesFence, Rule::RemovesFence]),
        (t, "fn f(s: Spawn) { s.with_env(SANDBOX_ENV, \"/\"); c.env(\"APPRAFTER_SANDBOX\", \"/\"); }", vec![Rule::SetsFence, Rule::SetsFence]),
        // None of these is a spawn of the CLI.
        (t, "fn f(s: Spawn) { s.without_env(\"XDG_DATA_HOME\"); c.env_remove(\"HCLOUD_TOKEN\"); }", vec![]),
        (t, "fn f(s: Spawn) { s.with_env(\"HOME\", p); c.env(\"NO_COLOR\", \"1\"); let _ = env[SANDBOX_ENV]; let _ = std::env::var_os(SANDBOX_ENV); }", vec![]),
        (t, "/// Mentions CARGO_BIN_EXE_apprafter and cargo_bin in a doc comment.\n#[doc = \"target/debug/apprafter\"]\nfn f() {}", vec![]),
        (t, "fn f() { let _ = \"bash-completion/completions/apprafter\"; let _ = \"/opt/bin/apprafter.exe\"; }", vec![]),
        (t, "fn f() { clap::Command::new(\"apprafter\").subcommand(x); }", vec![]),
        (s, "fn main() { std::env::current_exe(); c.env_clear(); }", vec![]),
        (t, "fn std_at_terminal(&self) {}", vec![]),
        // A `const fn` is a function: its body is walked as one.
        (t, "const fn f() { cargo_bin(); }", vec![Rule::CargoBin]),
    ] {
        assert_eq!(rules(file, src), want, "{file}: {src}");
    }
    // An allowance covers its own function and rule only.
    let golden = "platform-cli/tests/golden.rs";
    assert!(hits(
        golden,
        "fn harness_a_long_tmpdir_changes_no_golden() { std::env::current_exe(); }"
    )
    .is_empty());
    assert_eq!(
        rules(golden, "fn another() { std::env::current_exe(); }"),
        vec![Rule::CurrentExe]
    );
    assert_eq!(
        rules(
            golden,
            "fn harness_a_long_tmpdir_changes_no_golden() { c.env_clear(); }"
        ),
        vec![Rule::EnvClear]
    );
    // A hit names its line.
    assert_eq!(hits(t, "\n\nfn f() { cargo_bin(); }")[0].line, 3);
}
