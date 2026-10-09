// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The environment guard (ADR 0067 §2): nothing in `src/` reads or writes the process
//! environment except the files in [`ALLOWED`] — today `env.rs` alone, whose `AllowListEnv`
//! answers an allow-list of names and `None` for the rest, and which on Linux also reads the
//! display variables GTK reads and writes `WEBKIT_DISABLE_DMABUF_RENDERER`, WebKitGTK's own
//! switch, before any thread exists (its module docs).
//!
//! The scan reads syntax trees (`syn`), not text, so neither a comment nor a string can hide or
//! fake a hit. Test code is scanned too: a test reads its inputs through
//! `AllowListEnv::with_lookup` and never mutates the real environment. A file outside the
//! allow-list fails on:
//!
//! - a path ending in `env::var`, `env::var_os`, `env::vars`, `env::vars_os`, `env::set_var`,
//!   `env::remove_var` or `env::home_dir` (which reads `HOME`) ([`READS`]): `std::env::var`,
//!   `::std::env::var`, `env::var` after an import, a function pointer taken from one;
//! - a `use` that imports `std::env` or anything under it, imports or renames `std` itself, or
//!   globs `std::*`, in any form (groups, `self`, renames): each brings `env` into scope under a
//!   name the path rule cannot see (`use std::env as e; e::var("X")`);
//! - `extern crate std`, for the same reason.
//!
//! A macro's body, attribute arguments and tokens `syn` keeps verbatim are scanned token by
//! token: every `a::b::c` run of identifiers is a path, and a `use …;` or `extern crate …;` run is
//! parsed as the item it is. (Each rule is lexical, so the tokens are all it needs, whatever the
//! body would parse as.) `env!` and `option_env!` read at compile time and pass.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Span, TokenStream, TokenTree};
use syn::visit::{self, Visit};
use syn::{Expr, Item, UseTree};

/// The files under `src/` that may read the environment. WebKitGTK's rendering workaround,
/// set through the environment before GTK starts, lives in `env.rs` too: one file to audit.
const ALLOWED: [&str; 1] = ["env.rs"];

/// The `std::env` functions that read or write variables, by name; `home_dir` reads `HOME`.
const READS: [&str; 7] = [
    "var",
    "var_os",
    "vars",
    "vars_os",
    "set_var",
    "remove_var",
    "home_dir",
];

/// One way a file reaches the environment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Hit {
    line: usize,
    what: String,
}

/// Every [`Hit`] in `src`, in line order.
fn scan(src: &str) -> syn::Result<Vec<Hit>> {
    let file = syn::parse_file(src)?;
    let mut scanner = Scanner::default();
    scanner.visit_file(&file);
    let mut hits = scanner.hits;
    hits.sort();
    hits.dedup();
    Ok(hits)
}

fn line_of(span: Span) -> usize {
    span.start().line
}

/// `segs` ends in `env::<a read or write>`.
fn names_a_read(segs: &[String]) -> bool {
    matches!(segs, [.., env, f] if env == "env" && READS.contains(&f.as_str()))
}

/// What a `use` tree imports that brings `std::env` into scope: `std::env` or anything under
/// it, `std` itself (renamed or not), or a glob over either. `prefix` is the path so far.
fn std_env_imports(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<String>) {
    let std_env =
        |path: &[String]| path == ["std"] || path.starts_with(&["std".into(), "env".into()]);
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            std_env_imports(&p.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Name(n) => {
            let mut path = prefix.clone();
            if n.ident != "self" {
                path.push(n.ident.to_string());
            }
            if std_env(&path) {
                out.push(path.join("::"));
            }
        }
        UseTree::Rename(r) => {
            let mut path = prefix.clone();
            if r.ident != "self" {
                path.push(r.ident.to_string());
            }
            if std_env(&path) {
                out.push(format!("{} as {}", path.join("::"), r.rename));
            }
        }
        UseTree::Glob(_) => {
            if std_env(prefix) {
                out.push(format!("{}::*", prefix.join("::")));
            }
        }
        UseTree::Group(g) => {
            for t in &g.items {
                std_env_imports(t, prefix, out);
            }
        }
    }
}

#[derive(Default)]
struct Scanner {
    hits: Vec<Hit>,
}

impl Scanner {
    fn hit(&mut self, span: Span, what: String) {
        self.hits.push(Hit {
            line: line_of(span),
            what,
        });
    }

    /// Tokens that are no syntax tree: a `use …;` or `extern crate …;` run is parsed as that
    /// item; otherwise every `a::b::c` run of identifiers is a path.
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
                    if id == "use" || id == "extern" {
                        let end = (i..tts.len()).find(|&j| is_punct(j, ';'));
                        let item = end.and_then(|end| {
                            syn::parse2::<Item>(tts[i..=end].iter().cloned().collect()).ok()
                        });
                        if let (Some(end), Some(item)) = (end, item) {
                            self.visit_item(&item);
                            i = end + 1;
                            continue;
                        }
                    }
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
                    if names_a_read(&segs) {
                        self.hit(span, segs.join("::"));
                    }
                    i = j;
                }
                TokenTree::Punct(_) | TokenTree::Literal(_) => i += 1,
            }
        }
    }
}

impl<'ast> Visit<'ast> for Scanner {
    fn visit_path(&mut self, p: &'ast syn::Path) {
        let segs: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
        if names_a_read(&segs) {
            let lead = if p.leading_colon.is_some() { "::" } else { "" };
            let span = p
                .segments
                .first()
                .map_or_else(Span::call_site, |s| s.ident.span());
            self.hit(span, format!("{lead}{}", segs.join("::")));
        }
        visit::visit_path(self, p);
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        let mut found = Vec::new();
        std_env_imports(&u.tree, &mut Vec::new(), &mut found);
        for what in found {
            self.hit(u.use_token.span, format!("use {what}"));
        }
        visit::visit_item_use(self, u);
    }

    fn visit_item_extern_crate(&mut self, e: &'ast syn::ItemExternCrate) {
        if e.ident == "std" {
            self.hit(e.extern_token.span, "extern crate std".into());
        }
        visit::visit_item_extern_crate(self, e);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        visit::visit_macro(self, mac);
        self.scan_tokens(mac.tokens.clone());
    }

    fn visit_meta_list(&mut self, m: &'ast syn::MetaList) {
        visit::visit_meta_list(self, m);
        self.scan_tokens(m.tokens.clone());
    }

    fn visit_expr(&mut self, e: &'ast Expr) {
        if let Expr::Verbatim(tokens) = e {
            self.scan_tokens(tokens.clone());
        }
        visit::visit_expr(self, e);
    }

    fn visit_item(&mut self, item: &'ast Item) {
        if let Item::Verbatim(tokens) = item {
            self.scan_tokens(tokens.clone());
        }
        visit::visit_item(self, item);
    }
}

// ---------------------------------------------------------------------
// The scanner, on strings
// ---------------------------------------------------------------------

fn whats(src: &str) -> Vec<String> {
    scan(src)
        .unwrap_or_else(|e| panic!("parse {src:?}: {e}"))
        .into_iter()
        .map(|h| h.what)
        .collect()
}

#[test]
fn every_read_and_write_is_caught_however_it_is_qualified() {
    for f in READS {
        for path in [
            format!("std::env::{f}"),
            format!("::std::env::{f}"),
            format!("env::{f}"),
        ] {
            let src = format!("fn f() {{ let _ = {path}; }}");
            assert_eq!(whats(&src), std::slice::from_ref(&path), "{src}");
        }
    }
}

#[test]
fn home_dir_is_caught_because_it_reads_home() {
    for path in [
        "std::env::home_dir",
        "::std::env::home_dir",
        "env::home_dir",
    ] {
        let src = format!("fn f() {{ let _ = {path}(); }}");
        assert_eq!(whats(&src), [path], "{src}");
    }
    assert_eq!(
        whats("fn f() { println!(\"{:?}\", std::env::home_dir()); }"),
        ["std::env::home_dir"],
        "in a macro's tokens too"
    );
}

#[test]
fn an_aliased_import_is_caught_by_the_use_rule() {
    let src = "use std::env as e;\nfn f() { let _ = e::var(\"X\"); }";
    assert_eq!(
        scan(src).unwrap(),
        [Hit {
            line: 1,
            what: "use std::env as e".into()
        }]
    );
}

#[test]
fn every_form_of_importing_std_env_is_caught() {
    for (src, expected) in [
        ("use std::env;", &["use std::env"][..]),
        ("use ::std::env;", &["use std::env"]),
        ("pub(crate) use std::env;", &["use std::env"]),
        ("use std::env::var;", &["use std::env::var"]),
        (
            "use std::env::{var as v, vars};",
            &["use std::env::var as v", "use std::env::vars"],
        ),
        ("use std::{env, fs};", &["use std::env"]),
        ("use std::{fs, env::{self as e}};", &["use std::env as e"]),
        ("use std::env::{self};", &["use std::env"]),
        ("use std::env::*;", &["use std::env::*"]),
        ("use std::*;", &["use std::*"]),
        ("use std as s;", &["use std as s"]),
        ("use std::{self as s};", &["use std as s"]),
        ("use std;", &["use std"]),
        (
            "fn f() { use std::env::var_os; }",
            &["use std::env::var_os"],
        ),
        ("mod m { use std::env; }", &["use std::env"]),
        ("extern crate std;", &["extern crate std"]),
        ("extern crate std as s;", &["extern crate std"]),
    ] {
        assert_eq!(whats(src), expected, "{src}");
    }
}

#[test]
fn a_read_in_tokens_is_caught() {
    for (src, expected) in [
        // Arguments.
        (
            "fn f() { println!(\"{:?}\", std::env::var(\"X\")); }",
            "std::env::var",
        ),
        (
            "fn f() { assert!(format!(\"{:?}\", std::env::var_os(\"X\")).is_empty()); }",
            "std::env::var_os",
        ),
        // Statements, with an aliased import.
        (
            "m! { use std::env as e; let _ = e::var(\"X\"); }",
            "use std::env as e",
        ),
        // Neither expressions nor statements.
        (
            "fn f() { tracing::info!(target: \"t\", \"{:?}\", std::env::vars()); }",
            "std::env::vars",
        ),
        (
            "macro_rules! m { () => { std::env::set_var(\"A\", \"B\") }; }",
            "std::env::set_var",
        ),
        (
            "macro_rules! m { () => {{ use std::env as e; e::var(\"X\") }}; }",
            "use std::env as e",
        ),
        (
            "macro_rules! m { () => {{ extern crate std as s; use s::env as e; e::var(\"X\") }}; }",
            "extern crate std",
        ),
        // Attribute arguments.
        (
            "#[my_attr(std::env::remove_var(\"X\"))] fn f() {}",
            "std::env::remove_var",
        ),
        // Syntax `syn` keeps verbatim: a macro 2.0 item, a `become` expression.
        ("macro m() { std::env::var(\"X\") }", "std::env::var"),
        (
            "fn f() -> R { become std::env::var_os(\"X\") }",
            "std::env::var_os",
        ),
    ] {
        assert_eq!(whats(src), [expected], "{src}");
    }
}

#[test]
fn a_hit_names_its_line() {
    let src = "fn a() {}\n\nfn b() {\n    let _ = ::std::env::var(\"X\");\n}\n";
    assert_eq!(
        scan(src).unwrap(),
        [Hit {
            line: 4,
            what: "::std::env::var".into()
        }]
    );
}

#[test]
fn compile_time_reads_comments_strings_and_other_paths_pass() {
    let src = r#"
        //! Reads nothing: std::env::var("X").
        use std::{fs, io};
        use crate::env::AllowListEnv;
        /// Not std::env::var either.
        const V: &str = env!("CARGO_PKG_VERSION");
        const O: Option<&str> = option_env!("X");
        // std::env::var("X")
        const S: &str = "std::env::var";
        fn f(e: &dyn apprafter_core::EnvSource) {
            let _ = e.var("X");
            let _ = apprafter_core::EnvSource::var(e, "X");
            let _ = crate::env::data_dir_override;
            println!("{}", env!("CARGO_PKG_NAME"));
        }
    "#;
    assert_eq!(whats(src), Vec::<String>::new());
}

// ---------------------------------------------------------------------
// The guard, on the tree
// ---------------------------------------------------------------------

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `dir`, as (path relative to `dir` with `/` separators, full path).
fn rust_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap_or_else(|e| panic!("read {d:?}: {e}")) {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel, p));
            }
        }
    }
    out.sort();
    out
}

fn scan_file(path: &Path) -> Vec<Hit> {
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    scan(&src).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

#[test]
fn nothing_outside_the_allow_list_reads_the_environment() {
    let files = rust_files(&src_dir());
    assert!(
        files.iter().any(|(rel, _)| rel == "lib.rs"),
        "no src/lib.rs under {:?}",
        src_dir()
    );
    assert!(
        files.iter().any(|(rel, _)| rel.contains('/')),
        "the walk never went below src/: {files:?}"
    );
    let findings: Vec<String> = files
        .iter()
        .filter(|(rel, _)| !ALLOWED.contains(&rel.as_str()))
        .flat_map(|(rel, path)| {
            scan_file(path)
                .into_iter()
                .map(move |h| format!("src/{rel}:{}: {}", h.line, h.what))
        })
        .collect();
    assert!(
        findings.is_empty(),
        "read the environment only through src/env.rs (AllowListEnv):\n{}",
        findings.join("\n")
    );
}

/// An entry names a file that exists and that the scanner finds reads in — so the exemption is
/// not stale, and a scanner gone blind on real files fails here.
#[test]
fn every_allow_listed_file_exists_and_its_reads_are_seen() {
    for rel in ALLOWED {
        assert!(
            !scan_file(&src_dir().join(rel)).is_empty(),
            "src/{rel} is allow-listed but reads nothing"
        );
    }
}
