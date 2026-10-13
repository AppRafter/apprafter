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

// ---------------------------------------------------------------------
// G3 (D.4a T6, T8): real-environment contexts in the desktop's tests
// ---------------------------------------------------------------------

/// What builds a context from, or looks a location up in, the real environment, as the last
/// segments of a value path; any `dirs::*` too.
const REAL_ENV_BUILDERS: &[&[&str]] = &[
    &["Context", "from_cli_env"],
    &["Context", "from_desktop_env"],
    &["AllowListEnv", "from_process"],
    &["desktop_context"],
    &["home_dir"],
];

/// What reads or writes a store's secrets, a key or a cache (as `apprafter-core/tests/guards.rs`
/// lists them).
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
];

/// Openers called as a method of the context, by name (as `apprafter-core/tests/guards.rs`
/// lists them): `Context::secrets`.
const OPENER_METHODS: &[&str] = &["secrets"];

/// The desktop's tests allowed a real-environment builder: each compares values and opens nothing.
/// `real_api_base` is a helper, not a `#[test]`: a call to it counts as a builder in its caller,
/// which must be listed too and open nothing (critic finding 10).
const REAL_ENV_TESTS: &[(&str, &str)] = &[
    (
        "src/env.rs",
        "from_process_reads_the_real_environment_through_the_list",
    ),
    ("src/env.rs", "real_api_base"),
    (
        "src/env.rs",
        "a_release_context_ignores_the_inherited_token_and_any_api_base",
    ),
    (
        "src/env.rs",
        "a_test_build_context_honours_a_loopback_api_base_and_still_ignores_the_token",
    ),
    (
        "src/env.rs",
        "a_test_build_context_refuses_a_non_loopback_api_base",
    ),
    (
        "src/env.rs",
        "a_test_build_context_without_an_api_base_uses_the_real_one",
    ),
];

/// `cfg(test)`, or `cfg(all(…))` with `test` among its predicates.
fn cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let Ok(list) = a.meta.require_list() else {
            return false;
        };
        if !list.path.is_ident("cfg") {
            return false;
        }
        let tokens: Vec<TokenTree> = list.tokens.clone().into_iter().collect();
        match tokens.as_slice() {
            [TokenTree::Ident(t)] => t == "test",
            [TokenTree::Ident(all), TokenTree::Group(g)] if all == "all" => g
                .stream()
                .into_iter()
                .any(|t| matches!(t, TokenTree::Ident(i) if i == "test")),
            _ => false,
        }
    })
}

/// (function, line, opener, what) for each builder or opener in a file's test code; and every
/// function of its test code.
#[derive(Default)]
struct RealEnv {
    /// The listed names whose call counts as a builder.
    helpers: Vec<String>,
    whole: bool,
    depth: usize,
    fns: Vec<String>,
    uses: Vec<(String, usize, bool, String)>,
    sites: std::collections::HashSet<String>,
}

impl RealEnv {
    fn in_test(&self) -> bool {
        self.whole || self.depth > 0
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
        let hit = if segs.len() >= 2 && segs[0] == "dirs" {
            Some((false, segs.join("::")))
        } else if let Some(p) = REAL_ENV_BUILDERS.iter().find(|p| ends(p)) {
            Some((false, p.join("::")))
        } else if let Some(h) = self.helpers.iter().find(|h| segs.last() == Some(*h)) {
            Some((false, format!("{h} (a listed helper)")))
        } else {
            OPENERS
                .iter()
                .find(|p| ends(p))
                .map(|p| (true, p.join("::")))
        };
        if let Some((opener, what)) = hit {
            let site = self.fns.last().cloned().unwrap_or_default();
            self.uses.push((site, span.start().line, opener, what));
        }
    }
}

impl<'ast> Visit<'ast> for RealEnv {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let test = usize::from(cfg_test(&m.attrs));
        self.depth += test;
        visit::visit_item_mod(self, m);
        self.depth -= test;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let test = usize::from(cfg_test(&f.attrs));
        self.depth += test;
        if self.in_test() {
            self.sites.insert(f.sig.ident.to_string());
        }
        self.fns.push(f.sig.ident.to_string());
        visit::visit_item_fn(self, f);
        self.fns.pop();
        self.depth -= test;
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
            let site = self.fns.last().cloned().unwrap_or_default();
            self.uses
                .push((site, m.method.span().start().line, true, name));
        }
        visit::visit_expr_method_call(self, m);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        use syn::punctuated::Punctuated;
        if let Ok(args) =
            mac.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        {
            for e in &args {
                self.visit_expr(e);
            }
        } else if let Ok(stmts) = mac.parse_body_with(syn::Block::parse_within) {
            for s in &stmts {
                self.visit_stmt(s);
            }
        }
    }
}

fn real_env(src: &str, whole: bool, helpers: &[&str]) -> RealEnv {
    let file = syn::parse_file(src).unwrap();
    let mut scan = RealEnv {
        helpers: helpers.iter().map(|h| h.to_string()).collect(),
        whole: whole || cfg_test(&file.attrs),
        ..RealEnv::default()
    };
    scan.visit_file(&file);
    scan
}

#[test]
fn g3_sees_builders_and_openers_in_test_code_only() {
    let src = "\
fn production() { let _ = AllowListEnv::from_process(false); }
#[cfg(test)]
mod tests {
    fn t() {
        let ctx = Context::from_desktop_env(&env, policy, host()).unwrap();
        let _ = apprafter_core::runtime::materialise_kubeconfig(&ctx, &r);
        assert!(desktop_context(&e, h).is_ok());
    }
}
";
    let got: Vec<(String, bool, String)> = real_env(src, false, &[])
        .uses
        .into_iter()
        .map(|(site, _, opener, what)| (site, opener, what))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "t".to_string(),
                false,
                "Context::from_desktop_env".to_string()
            ),
            ("t".to_string(), true, "materialise_kubeconfig".to_string()),
            ("t".to_string(), false, "desktop_context".to_string()),
        ]
    );
}

/// D.4a b: the secret store is an opener here too, called as a method of the context.
#[test]
fn g3_sees_the_secret_store_called_as_a_method() {
    let src = "\
#[cfg(test)]
mod tests {
    fn t() {
        let ctx = Context::from_desktop_env(&env, policy, host()).unwrap();
        let _ = ctx.secrets().stored_token(\"p\");
        let _ = ctx.store();
    }
}
";
    let got: Vec<(String, bool, String)> = real_env(src, false, &[])
        .uses
        .into_iter()
        .map(|(site, _, opener, what)| (site, opener, what))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "t".to_string(),
                false,
                "Context::from_desktop_env".to_string()
            ),
            ("t".to_string(), true, "secrets".to_string()),
        ]
    );
}

/// D.4a c2: the lock state's file and the marker are openers (as `apprafter-core/tests/guards.rs`
/// lists them).
#[test]
fn g3_sees_the_lock_files_as_openers() {
    let src = "\
#[cfg(test)]
mod tests {
    fn t() {
        let ctx = Context::from_desktop_env(&env, policy, host()).unwrap();
        let _ = apprafter_core::secrets::read_store_file(ctx.config_root());
        let _ = apprafter_core::secrets::marker::write_marker(&ctx.store(), \"p\", &id);
        let _ = apprafter_core::secrets::testing::locked_store(ctx.config_root(), &[]);
    }
}
";
    let got: Vec<(String, bool, String)> = real_env(src, false, &[])
        .uses
        .into_iter()
        .map(|(site, _, opener, what)| (site, opener, what))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "t".to_string(),
                false,
                "Context::from_desktop_env".to_string()
            ),
            ("t".to_string(), true, "read_store_file".to_string()),
            ("t".to_string(), true, "write_marker".to_string()),
            ("t".to_string(), true, "locked_store".to_string()),
        ]
    );
}

/// Critic finding 10: a listed helper's caller is held to the builder rules.
#[test]
fn g3_counts_a_call_to_a_listed_helper_as_a_builder() {
    let src = "\
#[cfg(test)]
mod tests {
    fn t() {
        let base = real_api_base();
        let _ = cli_core::load_target(&store, \"x\");
    }
}
";
    let got: Vec<(String, bool, String)> = real_env(src, false, &["real_api_base"])
        .uses
        .into_iter()
        .map(|(site, _, opener, what)| (site, opener, what))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "t".to_string(),
                false,
                "real_api_base (a listed helper)".to_string()
            ),
            ("t".to_string(), true, "load_target".to_string()),
        ]
    );
}

#[test]
fn real_environment_contexts_in_the_desktops_tests_stay_where_listed_and_open_nothing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    let mut sites = std::collections::HashMap::new();
    // Every listed name: a `#[test]` is never called, so only a helper's name matches a call.
    let helpers: Vec<&str> = REAL_ENV_TESTS.iter().map(|&(_, site)| site).collect();
    for (dir, whole) in [("src", false), ("tests", true)] {
        for path in rust_files(&root.join(dir)) {
            let file = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let scan = real_env(&fs::read_to_string(&path).unwrap(), whole, &helpers);
            for (site, line, opener, what) in scan.uses {
                found.push((file.clone(), site, line, opener, what));
            }
            sites.insert(file, scan.sites);
        }
    }
    let listed =
        |file: &str, site: &str| REAL_ENV_TESTS.iter().any(|(f, s)| *f == file && *s == site);
    let bad: Vec<String> = found
        .iter()
        .filter(|(file, site, _, opener, _)| listed(file, site) == *opener)
        .map(|(file, site, line, opener, what)| {
            let why = if *opener {
                "opens"
            } else {
                "builds from the real environment"
            };
            format!("{file}:{line}: `{what}` in `{site}` {why}")
        })
        .collect();
    assert!(
        bad.is_empty(),
        "a desktop test builds a context from the real environment outside REAL_ENV_TESTS, or a \
         listed one opens a secret, a key or a cache (D.4a T6, G3):\n{}",
        bad.join("\n")
    );
    for (file, site) in REAL_ENV_TESTS {
        assert!(
            sites.get(*file).is_some_and(|s| s.contains(*site)),
            "REAL_ENV_TESTS names {file} `{site}`, which is no test function there"
        );
        assert!(
            found.iter().any(|(f, s, _, opener, _)| f == file && s == site && !opener),
            "REAL_ENV_TESTS names {file} `{site}`, which no longer builds from the real environment"
        );
    }
}
