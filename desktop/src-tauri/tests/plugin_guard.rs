// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Only the app builds a real plugin (GOTCHA-156). The plugins reach the owner's session — the
//! clipboard plugin's setup connects to the display's clipboard, the opener starts a browser,
//! the single-instance plugin claims a name on the session bus — and Tauri's mock runtime runs a
//! plugin's setup like the app does. So outside the app itself (`src/app.rs`, which builds them,
//! and `src/lib.rs`, which registers them) no file under `src/` or `tests/` may name a plugin
//! crate (`tauri_plugin_*`) or the app's builders of one (`clipboard_plugin`, `opener_plugin`).
//! The rig registers stand-ins instead (tests/common/plugins.rs) and checks every app it builds
//! (`common::rig_on`); two exceptions are named here:
//!
//! - the plugins' handle types, `tauri_plugin_clipboard_manager::Clipboard` and
//!   `tauri_plugin_opener::Opener`, which the rig asks the app for to show that neither set up;
//! - tests/opener_scope.rs's `opener_plugin`, built in a child process with no way to start
//!   anything (its docs).
//!
//! The scan reads tokens (`proc_macro2`), so a comment never counts and a string never hides a
//! name; a `use` of a plugin crate, renamed, grouped or globbed, is a name of it too.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};

/// The files that build or register the real plugins: the app.
const APP: [&str; 2] = ["src/app.rs", "src/lib.rs"];

/// The app's builders of a real plugin; and the one other file that may name one, with that one.
const BUILDERS: [&str; 2] = ["clipboard_plugin", "opener_plugin"];
const SEALED: (&str, &str) = ("tests/opener_scope.rs", "opener_plugin");

/// What a file may name a plugin crate for: its handle type, as `<crate>::<type>`.
const HANDLES: [(&str, &str); 2] = [
    ("tauri_plugin_clipboard_manager", "Clipboard"),
    ("tauri_plugin_opener", "Opener"),
];

/// Every name of a real plugin in `src` that `file` (relative to the crate) may not make, with
/// its line.
fn hits(file: &str, src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    if APP.contains(&file) {
        return out;
    }
    let tokens: TokenStream = src.parse().unwrap_or_else(|e| panic!("{file}: {e}"));
    walk(file, tokens, &mut out);
    out
}

fn walk(file: &str, tokens: TokenStream, out: &mut Vec<(usize, String)>) {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Group(group) => walk(file, group.stream(), out),
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                let builder = BUILDERS.contains(&name.as_str()) && (file, name.as_str()) != SEALED;
                let krate =
                    name.starts_with("tauri_plugin_") && !names_a_handle(&name, &tokens[i + 1..]);
                if builder || krate {
                    out.push((ident.span().start().line, name));
                }
            }
            _ => {}
        }
    }
}

/// `crate` followed by `::<its handle type>` (and nothing that makes it a group or a glob).
fn names_a_handle(krate: &str, rest: &[TokenTree]) -> bool {
    let Some(&(_, handle)) = HANDLES.iter().find(|(k, _)| *k == krate) else {
        return false;
    };
    match rest {
        [TokenTree::Punct(a), TokenTree::Punct(b), TokenTree::Ident(ty), after @ ..] => {
            a.as_char() == ':'
                && a.spacing() == Spacing::Joint
                && b.as_char() == ':'
                && *ty == handle
                && !matches!(after.first(), Some(TokenTree::Punct(p)) if p.as_char() == ':')
                && !matches!(after.first(), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace)
        }
        _ => false,
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_the_app_builds_a_real_plugin() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    files.sort();
    let mut found = Vec::new();
    for path in &files {
        let file = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for (line, name) in hits(&file, &fs::read_to_string(path).unwrap()) {
            found.push(format!("{file}:{line}: {name}"));
        }
    }
    assert!(
        found.is_empty(),
        "a real plugin outside the app:\n{}",
        found.join("\n")
    );
    // The scan saw the files it guards: the rig, and the app's own.
    for file in [
        "tests/common/mod.rs",
        "tests/common/plugins.rs",
        "src/app.rs",
    ] {
        assert!(
            files.iter().any(|p| p.ends_with(file)),
            "{file} not scanned"
        );
    }
}

/// The rules on sources of their own: each way to name a plugin is found, and what is not one
/// is not.
#[test]
fn the_scan_finds_every_way_to_build_a_plugin_and_nothing_else() {
    let found = |src: &str| {
        hits("tests/x.rs", src)
            .into_iter()
            .map(|(_, n)| n)
            .collect::<Vec<_>>()
    };
    for (src, name) in [
        (
            "fn f() { b.plugin(app::clipboard_plugin()); }",
            "clipboard_plugin",
        ),
        ("fn f() { b.plugin(opener_plugin()); }", "opener_plugin"),
        (
            "use apprafter_desktop::app::clipboard_plugin as c;",
            "clipboard_plugin",
        ),
        (
            "fn f() { tauri_plugin_clipboard_manager::init(); }",
            "tauri_plugin_clipboard_manager",
        ),
        (
            "fn f() { tauri_plugin_opener::Builder::new(); }",
            "tauri_plugin_opener",
        ),
        (
            "fn f() { tauri_plugin_single_instance::init(|_, _, _| {}); }",
            "tauri_plugin_single_instance",
        ),
        (
            "use tauri_plugin_opener::{Builder, Opener};",
            "tauri_plugin_opener",
        ),
        ("use tauri_plugin_opener as o;", "tauri_plugin_opener"),
        (
            "use tauri_plugin_clipboard_manager::*;",
            "tauri_plugin_clipboard_manager",
        ),
        (
            "fn f() { tauri_plugin_opener::Opener::new(); }",
            "tauri_plugin_opener",
        ),
        (
            "fn f() { tauri_plugin_notification::init(); }",
            "tauri_plugin_notification",
        ),
        (
            "macro_rules! m { () => { tauri_plugin_opener::init() } }",
            "tauri_plugin_opener",
        ),
    ] {
        assert_eq!(found(src), [name], "{src}");
    }
    for src in [
        "// b.plugin(app::clipboard_plugin());\nfn f() {}",
        "/// `tauri_plugin_opener::init()`\nfn f() {}",
        r#"const S: &str = "tauri_plugin_clipboard_manager::init clipboard_plugin";"#,
        "fn f(a: &App) { a.try_state::<tauri_plugin_clipboard_manager::Clipboard<MockRuntime>>(); }",
        "fn f(a: &App) { a.try_state::<tauri_plugin_opener::Opener<MockRuntime>>(); }",
    ] {
        assert_eq!(found(src), Vec::<String>::new(), "{src}");
    }
    // The app's files and the sealed probe may.
    assert!(hits(
        "src/lib.rs",
        "fn f() { b.plugin(app::clipboard_plugin()); }"
    )
    .is_empty());
    assert!(hits(SEALED.0, "fn f() { b.plugin(app::opener_plugin()); }").is_empty());
    assert!(!hits(SEALED.0, "fn f() { b.plugin(app::clipboard_plugin()); }").is_empty());
}
