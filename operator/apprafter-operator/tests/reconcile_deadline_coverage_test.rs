// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every controller this operator runs is bounded by a reconcile deadline
//! (WI-400).
//!
//! kube-runtime holds every later trigger for an object while a pass for it
//! is in flight (GOTCHA-51), so a `Controller` whose reconcile is not wrapped
//! in `operator_core::deadline::within` freezes its objects for as long as
//! one call hangs. Each controller crate pins its own run site with a test of
//! its own; this file pins the SET, so a new controller cannot arrive
//! unbounded:
//!
//! - in every crate of the workspace, the production code holds as many
//!   `deadline::within(` calls and `RECONCILE_DEADLINE` constants as
//!   `Controller::new(` calls;
//! - every deadline has its compile-time `< CLIENT_READ_TIMEOUT` assert in
//!   `src/lib.rs`;
//! - every deadline is a boundary of the reconcile-duration histogram, with
//!   a finite bucket above it, so a pass cut at it is not filed under `+Inf`.
//!
//! The scan reads source text. Comments, string and char literals, every
//! `#[cfg(test)]` item and the file of every `#[cfg(test)] mod x;` are
//! removed first, and so is all whitespace, so a test, a doc comment, a
//! needle assembled in a string or a line rustfmt wrapped cannot change a
//! count. It counts the qualified spelling: call the wrapper as
//! `operator_core::deadline::within(..)`, never as a bare `within(..)`
//! brought in by a `use`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use operator_core::metrics::RECONCILE_DURATION_BUCKETS;

/// How many `Controller::new(` the operator runs today. A new controller
/// raises it once its reconcile is wrapped in `deadline::within`, its
/// `RECONCILE_DEADLINE` is asserted against `CLIENT_READ_TIMEOUT` in
/// `src/lib.rs`, and it is listed in [`deadlines`].
const CONTROLLERS: usize = 9;

/// One controller's deadline: the kind it is reported as, the path of its
/// constant as `src/lib.rs` spells it, and its value.
macro_rules! deadline {
    ($kind:literal, $path:path) => {
        ($kind, stringify!($path), $path)
    };
}

/// Every controller's deadline.
fn deadlines() -> [(&'static str, &'static str, Duration); CONTROLLERS] {
    [
        deadline!(
            "Application",
            operator_controllers_application::RECONCILE_DEADLINE
        ),
        deadline!(
            "MigrationPlan",
            operator_controllers_migration::reconcile::RECONCILE_DEADLINE
        ),
        deadline!(
            "PlatformStack",
            operator_controllers_platform_stack::reconcile::RECONCILE_DEADLINE
        ),
        deadline!(
            "ResourceClaim (provisioner)",
            operator_controllers_resourceclaim_provisioner::reconcile::RECONCILE_DEADLINE
        ),
        deadline!(
            "ResourceClaim (scheduler)",
            operator_controllers_resourceclaim_scheduler::RECONCILE_DEADLINE
        ),
        deadline!(
            "RetainedClaim",
            operator_controllers_resourceclaim_provisioner::gc::RECONCILE_DEADLINE
        ),
        deadline!(
            "SharedDatabase",
            operator_controllers_resourceclaim_provisioner::shared_database::RECONCILE_DEADLINE
        ),
        deadline!(
            "SharedVolume",
            operator_controllers_resourceclaim_provisioner::shared_volume::RECONCILE_DEADLINE
        ),
        deadline!(
            "SourceCredential",
            operator_controllers_sourcecredential::RECONCILE_DEADLINE
        ),
    ]
}

/// `operator/`, the workspace root.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("apprafter-operator sits inside the operator workspace")
        .to_path_buf()
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read a source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The production source of every crate in the workspace, keyed by the
/// crate's directory relative to `operator/`: every `src/**/*.rs` except the
/// files of `#[cfg(test)]` modules, each passed through [`production_code`],
/// with all whitespace removed.
fn crates() -> BTreeMap<String, String> {
    let root = workspace();
    let mut dirs = Vec::new();
    for top in std::fs::read_dir(&root).expect("read operator/") {
        let top = top.expect("a directory entry").path();
        if top.join("Cargo.toml").is_file() && top.join("src").is_dir() {
            dirs.push(top);
        } else if top.is_dir() && top.file_name().is_some_and(|n| n == "operator-controllers") {
            for sub in std::fs::read_dir(&top).expect("read operator-controllers/") {
                let sub = sub.expect("a directory entry").path();
                if sub.join("Cargo.toml").is_file() && sub.join("src").is_dir() {
                    dirs.push(sub);
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    for dir in dirs {
        let mut files = Vec::new();
        rust_files(&dir.join("src"), &mut files);
        files.sort();
        let sources: Vec<(PathBuf, String)> = files
            .into_iter()
            .map(|f| {
                let source = std::fs::read_to_string(&f).expect("read a source file");
                (f, source)
            })
            .collect();
        let test_only: BTreeSet<PathBuf> = sources
            .iter()
            .flat_map(|(f, source)| test_only_files(f, source))
            .collect();
        let code: String = sources
            .iter()
            .filter(|(f, _)| !test_only.contains(f))
            .map(|(_, source)| squeezed(&production_code(source)))
            .collect();
        let name = dir
            .strip_prefix(&root)
            .expect("under operator/")
            .to_string_lossy()
            .into_owned();
        out.insert(name, code);
    }
    out
}

fn squeezed(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The files of the out-of-line modules that `source`, the file at `path`,
/// declares behind `#[cfg(test)]`. `#[cfg(test)] mod x;` is `x.rs` or
/// `x/mod.rs` beside a `lib.rs`, `main.rs` or `mod.rs`, and in a directory
/// named after any other file.
fn test_only_files(path: &Path, source: &str) -> Vec<PathBuf> {
    let parent = path.parent().expect("a source file has a directory");
    let dir = match path.file_stem().and_then(|s| s.to_str()) {
        Some("lib" | "main" | "mod") => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
        None => return Vec::new(),
    };
    let code = squeezed(&strip_comments_and_literals(source));
    let mut out = Vec::new();
    for marker in [
        "#[cfg(test)]mod",
        "#[cfg(test)]pubmod",
        "#[cfg(test)]pub(crate)mod",
    ] {
        for (at, _) in code.match_indices(marker) {
            let rest = &code[at + marker.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && rest[name.len()..].starts_with(';') {
                out.push(dir.join(format!("{name}.rs")));
                out.push(dir.join(&name).join("mod.rs"));
            }
        }
    }
    out
}

/// `source` with comments removed, string and char literals emptied, and
/// every item behind `#[cfg(test)]` dropped (a `mod x { .. }`, a `fn`, a
/// `mod x;` declaration, a `use`).
fn production_code(source: &str) -> String {
    let code = strip_comments_and_literals(source);
    let chars: Vec<char> = code.chars().collect();
    let marker: Vec<char> = "#[cfg(test)]".chars().collect();
    let mut out = String::with_capacity(code.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i..].starts_with(&marker) {
            i += marker.len();
            // The item ends at the first `;` or at the `}` that closes the
            // first `{` outside any `(..)` / `[..]` (a signature such as
            // `fn f(a: [u8; 2])` carries both).
            let mut nesting = 0usize;
            while i < chars.len() {
                match chars[i] {
                    '(' | '[' => nesting += 1,
                    ')' | ']' => nesting = nesting.saturating_sub(1),
                    ';' if nesting == 0 => {
                        i += 1;
                        break;
                    }
                    '{' if nesting == 0 => {
                        let mut depth = 0usize;
                        while i < chars.len() {
                            match chars[i] {
                                '{' => depth += 1,
                                '}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            i += 1;
                        }
                        i += 1;
                        break;
                    }
                    _ => {}
                }
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `source` without comments, with every string literal reduced to `""` and
/// every char literal to `' '`, so neither can contribute a brace or a needle.
fn strip_comments_and_literals(source: &str) -> String {
    let c: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    while i < c.len() {
        // Line comment (`//`, `///`, `//!`).
        if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // Block comment; Rust's nest.
        if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 0usize;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // Raw string: r"..", r#".."#, br"..", with any number of `#`.
        let starts_token = i == 0 || !ident(c[i - 1]);
        let r_at = if c[i] == 'r' && starts_token {
            Some(i)
        } else if c[i] == 'b' && c.get(i + 1) == Some(&'r') && starts_token {
            Some(i + 1)
        } else {
            None
        };
        if let Some(r) = r_at {
            let mut j = r + 1;
            while c.get(j) == Some(&'#') {
                j += 1;
            }
            if c.get(j) == Some(&'"') {
                let hashes = j - r - 1;
                j += 1;
                'raw: while j < c.len() {
                    if c[j] == '"' && (0..hashes).all(|k| c.get(j + 1 + k) == Some(&'#')) {
                        j += 1 + hashes;
                        break 'raw;
                    }
                    j += 1;
                }
                out.push_str("\"\"");
                i = j;
                continue;
            }
        }
        // String (and byte string: its `b` was already copied).
        if c[i] == '"' {
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            out.push_str("\"\"");
            continue;
        }
        // Char literal, told apart from a lifetime (`'a`, `'static`): an
        // escape, or exactly one char before the closing quote.
        if c[i] == '\'' {
            if c.get(i + 1) == Some(&'\\') {
                let mut j = i + 2;
                while j < c.len() && c[j] != '\'' {
                    j += 1;
                }
                out.push_str("' '");
                i = j + 1;
                continue;
            }
            if c.get(i + 2) == Some(&'\'') {
                out.push_str("' '");
                i += 3;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

#[test]
fn every_controller_runs_its_reconcile_under_a_deadline() {
    let crates = crates();
    let mut total = 0;
    for (name, code) in &crates {
        let controllers = code.matches("Controller::new(").count();
        let bounded = code.matches("deadline::within(").count();
        let defined = code.matches("constRECONCILE_DEADLINE:Duration=").count();
        total += controllers;
        assert_eq!(
            (bounded, defined),
            (controllers, controllers),
            "{name}: {controllers} Controller::new( against {bounded} deadline::within( and \
             {defined} `const RECONCILE_DEADLINE: Duration =` in its production code. Every \
             controller hands kube-runtime a reconcile wrapped in \
             operator_core::deadline::within(RECONCILE_DEADLINE, ..) under its own constant, \
             and nothing else in production code calls it."
        );
    }
    assert_eq!(
        total,
        CONTROLLERS,
        "the scan found {total} Controller::new( across {:?}; a new controller raises \
         CONTROLLERS once it is wired under a deadline",
        crates.keys().collect::<Vec<_>>()
    );
}

#[test]
fn every_deadline_is_asserted_against_the_client_read_timeout() {
    let lib = squeezed(&production_code(include_str!("../src/lib.rs")));
    let asserted = lib
        .matches(".as_secs()<CLIENT_READ_TIMEOUT.as_secs()")
        .count();
    assert_eq!(
        asserted, CONTROLLERS,
        "src/lib.rs holds one `const _: () = assert!(<path>::RECONCILE_DEADLINE.as_secs() < \
         CLIENT_READ_TIMEOUT.as_secs(), ..)` per controller; {asserted} found"
    );
    for (kind, path, _) in deadlines() {
        let needle = format!(
            "assert!({}.as_secs()<CLIENT_READ_TIMEOUT.as_secs(),",
            squeezed(path)
        );
        assert!(
            lib.contains(&needle),
            "{kind}: src/lib.rs has no `const _: () = assert!({path}.as_secs() < \
             CLIENT_READ_TIMEOUT.as_secs(), ..)`"
        );
    }
}

#[test]
fn every_deadline_is_a_reconcile_duration_bucket_boundary() {
    for (kind, _, deadline) in deadlines() {
        let secs = deadline.as_secs_f64();
        assert!(
            RECONCILE_DURATION_BUCKETS.contains(&secs),
            "{kind}: its {secs}s deadline is not a boundary of \
             apprafter_reconcile_duration_seconds ({RECONCILE_DURATION_BUCKETS:?})"
        );
        assert!(
            RECONCILE_DURATION_BUCKETS.iter().any(|b| *b > secs),
            "{kind}: a pass cut at {secs}s must land in a finite bucket, not +Inf"
        );
    }
}

/// The scanner itself: what it must drop and what it must keep.
#[test]
fn the_scan_sees_production_code_only() {
    let source = r##"
use x::y;
// Controller::new( in a comment
/// deadline::within( in a doc comment
/* Controller::new( /* nested */ still a comment */
fn run() {
    let s = "Controller::new(";
    let r = r#"deadline::within(" "#;
    let brace = '{';
    let life: &'static str = "";
    Controller::new(a, b).run(|o, c| operator_core::deadline::within(D, r(o, c)), e, x);
}
#[cfg(test)]
mod tests {
    fn t() { Controller::new(a, b); let close = '}'; }
}
#[cfg(test)]
mod route;
#[cfg(test)]
fn helper(a: [u8; 2]) { Controller::new(a); }
fn after() { deadline::within(D, f()); }
"##;
    let code = production_code(source);
    assert_eq!(code.matches("Controller::new(").count(), 1, "{code}");
    assert_eq!(code.matches("deadline::within(").count(), 2, "{code}");
    assert!(code.contains("fn after()"), "{code}");
    assert!(!code.contains("mod route"), "{code}");
}

/// A test-only module's own file is not production code: the provisioner's
/// route table, fake apiserver and allocation tests are declared this way.
#[test]
fn the_file_of_a_test_only_module_is_not_scanned() {
    let source = "pub mod gc;\n#[cfg(test)]\nmod route_apiserver;\n#[cfg(test)]\nmod tests {\n}\n\
                  // #[cfg(test)] mod commented;\n#[cfg(test)]\npub(crate) mod fixtures;\n";
    assert_eq!(
        test_only_files(Path::new("/w/c/src/lib.rs"), source),
        [
            "/w/c/src/route_apiserver.rs",
            "/w/c/src/route_apiserver/mod.rs",
            "/w/c/src/fixtures.rs",
            "/w/c/src/fixtures/mod.rs",
        ]
        .map(PathBuf::from)
    );
    assert_eq!(
        test_only_files(Path::new("/w/c/src/gc.rs"), "#[cfg(test)]\nmod fixtures;\n"),
        ["/w/c/src/gc/fixtures.rs", "/w/c/src/gc/fixtures/mod.rs"].map(PathBuf::from)
    );
}
