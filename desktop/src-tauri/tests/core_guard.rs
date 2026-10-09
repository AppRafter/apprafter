// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The desktop binds every target by name (ADR 0067 §2, `TargetRef::named`): no path in `src/`
//! reaches `TargetRef::active`, the CLI's `config.yaml` default. The core still reads that
//! pointer for a report (whoami says what the CLI's default is) — inside the core, not here.
//!
//! Syntax trees, not text: a comment or a string can neither fake nor hide a hit. It fails on a
//! path with `TargetRef` then `active`, on `<TargetRef>::active`, on a `use` that renames
//! `TargetRef` and on a `type` alias of it (either would let `T::active` through), and on the
//! same token run inside a macro's body.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Span, TokenStream, TokenTree};
use syn::visit::{self, Visit};

/// One way a file reaches the active pointer.
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

/// A type path whose last segment is `TargetRef`.
fn names_target_ref(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "TargetRef"))
}

#[derive(Default)]
struct Scanner {
    hits: Vec<Hit>,
}

impl Scanner {
    fn hit(&mut self, span: Span, what: impl Into<String>) {
        self.hits.push(Hit {
            line: span.start().line,
            what: what.into(),
        });
    }

    /// `TargetRef :: active` among a macro's tokens, at any depth.
    fn scan_tokens(&mut self, tokens: TokenStream) {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        for (i, tree) in trees.iter().enumerate() {
            if let TokenTree::Group(group) = tree {
                self.scan_tokens(group.stream());
                continue;
            }
            if let (
                TokenTree::Ident(a),
                Some(TokenTree::Punct(c1)),
                Some(TokenTree::Punct(c2)),
                Some(TokenTree::Ident(b)),
            ) = (tree, trees.get(i + 1), trees.get(i + 2), trees.get(i + 3))
            {
                if a == "TargetRef" && c1.as_char() == ':' && c2.as_char() == ':' && b == "active" {
                    self.hit(a.span(), "TargetRef::active in a macro");
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for Scanner {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if segs
            .windows(2)
            .any(|w| w[0] == "TargetRef" && w[1] == "active")
        {
            self.hit(path.segments[0].ident.span(), segs.join("::"));
        }
        visit::visit_path(self, path);
    }

    fn visit_expr_path(&mut self, e: &'ast syn::ExprPath) {
        if let Some(qself) = &e.qself {
            if names_target_ref(&qself.ty) && e.path.segments.iter().any(|s| s.ident == "active") {
                // syn 2: punctuation has `spans`, not `span`.
                self.hit(qself.lt_token.spans[0], "<TargetRef>::active");
            }
        }
        visit::visit_expr_path(self, e);
    }

    fn visit_use_rename(&mut self, r: &'ast syn::UseRename) {
        if r.ident == "TargetRef" {
            self.hit(r.ident.span(), format!("use TargetRef as {}", r.rename));
        }
        visit::visit_use_rename(self, r);
    }

    fn visit_item_type(&mut self, t: &'ast syn::ItemType) {
        if names_target_ref(&t.ty) {
            self.hit(t.ident.span(), format!("type {} = TargetRef", t.ident));
        }
        visit::visit_item_type(self, t);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        self.scan_tokens(m.tokens.clone());
        visit::visit_macro(self, m);
    }
}

/// Every `*.rs` below `dir`, sorted.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn the_scanner_sees_every_way_to_the_active_pointer() {
    for (src, line) in [
        (
            "fn f(c: &C) { let _ = apprafter_core::TargetRef::active(c); }",
            1,
        ),
        (
            "use apprafter_core::TargetRef;\nfn f(c: &C) { TargetRef::active(c).ok(); }",
            2,
        ),
        (
            "fn f() { let p = apprafter_core::target_ref::TargetRef::active; }",
            1,
        ),
        (
            "fn f(c: &C) { <apprafter_core::TargetRef>::active(c).ok(); }",
            1,
        ),
        (
            "use apprafter_core::TargetRef as T;\nfn f(c: &C) { T::active(c).ok(); }",
            1,
        ),
        ("type T = apprafter_core::TargetRef;", 1),
        (
            "fn f(c: &C) { println!(\"{:?}\", TargetRef::active(c)); }",
            1,
        ),
        (
            "fn f(c: &C) { assert!(matches!(TargetRef :: active(c), Ok(_))); }",
            1,
        ),
    ] {
        let hits = scan(src).unwrap();
        assert!(hits.iter().any(|h| h.line == line), "{src}: {hits:?}");
    }
}

#[test]
fn naming_a_target_comments_and_strings_pass() {
    for src in [
        "fn f(c: &C) { TargetRef::named(c, \"prod\").ok(); }",
        "// TargetRef::active is the CLI's default\nfn f() {}",
        "/// TargetRef::active is the CLI's default\nfn f() {}",
        "fn f() { let s = \"TargetRef::active\"; println!(\"TargetRef::active\"); }",
        "fn active() {} fn g() { active(); }",
        "use apprafter_core::TargetRef;\nfn f(r: &TargetRef) -> &str { r.name() }",
    ] {
        assert_eq!(scan(src).unwrap(), Vec::<Hit>::new(), "{src}");
    }
}

#[test]
fn no_file_under_src_reaches_the_active_pointer() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src);
    assert!(
        files.iter().any(|f| f.ends_with("target_ops.rs")),
        "the scan must reach the target commands: {files:?}"
    );
    let mut found = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for hit in scan(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display())) {
            found.push(format!("{}:{}: {}", file.display(), hit.line, hit.what));
        }
    }
    assert!(
        found.is_empty(),
        "the desktop binds targets by name:\n{}",
        found.join("\n")
    );
}
