// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Only the app builds a real plugin (GOTCHA-156). The plugins reach the owner's session — the
//! clipboard plugin's setup connects to the display's clipboard, the opener starts a browser,
//! the single-instance plugin claims a name on the session bus — and Tauri's mock runtime runs a
//! plugin's setup like the app does. So outside the app's own code (`src/app.rs`, which builds
//! them, `src/lib.rs`, which registers them, and `src/single_instance.rs`, which builds and
//! registers the single-instance plugin behind its check of the session bus — their
//! `#[cfg(test)]` and `#[cfg(all(test, …))]` modules are not the app, and are scanned like any
//! test) no file under `src/` or `tests/` may name a plugin crate (`tauri_plugin_*`) or the
//! app's builders of one (`clipboard_plugin`, `opener_plugin`, and `SingleInstance`, whose
//! `register` builds the single-instance plugin). The rig registers stand-ins instead
//! (tests/common/plugins.rs) and checks every app it builds (`common::rig_on`); the exceptions
//! are named here:
//!
//! - the plugins' handle types, `tauri_plugin_clipboard_manager::Clipboard` and
//!   `tauri_plugin_opener::Opener`, which the rig asks the app for to show that neither set up;
//! - the functions in [`ALLOWED`], each for one builder: an app test that builds the opener
//!   without setting it up, tests/sealed_plugins.rs's probes, which build a real plugin in a
//!   child process with no way to reach anything (its docs) and must start by checking that
//!   seal, tests/single_instance.rs's probe, which runs the lock's wiring in a child process
//!   whose only bus is one its parent test gave it (its docs), and src/single_instance.rs's
//!   tests of the decision, which register nothing.
//!
//! The scan reads tokens (`proc_macro2`), so a comment never counts and a string never hides a
//! name; a `use` of a plugin crate, renamed, grouped or globbed, is a name of it too, and so is a
//! raw identifier (`r#tauri_plugin_opener`), which rustc reads as the plain one.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};

/// The files that build or register the real plugins: the app. Only their test modules
/// (`#[cfg(test)]`, `#[cfg(all(test, …))]`) are scanned.
const APP: [&str; 3] = ["src/app.rs", "src/lib.rs", "src/single_instance.rs"];

/// The app's builders of a real plugin. For the single-instance plugin, the type whose
/// `register` builds it: the method's name is too common to scan for (signals.rs's
/// `low_level::register`, target_ops.rs's own), and a value to call it on is reached only
/// through the type (`SingleInstance::for_this_launch`, `on_session_bus`, `On`).
const BUILDERS: [&str; 3] = ["clipboard_plugin", "opener_plugin", "SingleInstance"];

/// Who may name a builder: the file, the function, the builder, and whether the function must
/// start with `sealed();` (the probe's check that its process cannot reach the session).
const ALLOWED: [(&str, &str, &str, bool); 6] = [
    // Reads the opener's scripts; nothing builds an app with it, so no setup runs.
    (
        "src/app.rs",
        "the_opener_plugin_injects_no_script",
        "opener_plugin",
        false,
    ),
    (
        "tests/sealed_plugins.rs",
        "opener_probe",
        "opener_plugin",
        true,
    ),
    (
        "tests/sealed_plugins.rs",
        "clipboard_probe",
        "clipboard_plugin",
        true,
    ),
    // The lock's wiring, the real plugin included, in a child process its parent test starts
    // with the environment cleared, `HOME` and `XDG_RUNTIME_DIR` in a temporary directory, and
    // a bus address that is no bus, a socket in that directory or a private dbus-daemon (its
    // docs). It needs that bus, so it cannot be sealed; it starts by checking it is that child.
    (
        "tests/single_instance.rs",
        "wiring_probe",
        "SingleInstance",
        false,
    ),
    // The decision alone, on stub connects: nothing builds an app, so nothing registers.
    (
        "src/single_instance.rs",
        "an_address_that_does_not_parse_is_off_without_connecting",
        "SingleInstance",
        false,
    ),
    (
        "src/single_instance.rs",
        "an_address_that_parses_is_on_exactly_when_the_bus_answers",
        "SingleInstance",
        false,
    ),
];

/// What a file may name a plugin crate for: its handle type, as `<crate>::<type>`.
const HANDLES: [(&str, &str); 2] = [
    ("tauri_plugin_clipboard_manager", "Clipboard"),
    ("tauri_plugin_opener", "Opener"),
];

/// Every name of a real plugin in `src` that `file` (relative to the crate) may not make, with
/// its line.
fn hits(file: &str, src: &str) -> Vec<(usize, String)> {
    let tokens: TokenStream = src.parse().unwrap_or_else(|e| panic!("{file}: {e}"));
    let mut out = Vec::new();
    if APP.contains(&file) {
        for module in test_modules(tokens) {
            walk(file, None, module, &mut out);
        }
    } else {
        walk(file, None, tokens, &mut out);
    }
    out
}

/// The bodies of the `#[cfg(test)] mod <name> { … }` items at the top of a file (and of those
/// behind `cfg(all(test, …))`).
fn test_modules(tokens: TokenStream) -> Vec<TokenStream> {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    let mut out = Vec::new();
    for window in tokens.windows(5) {
        if let [TokenTree::Punct(hash), TokenTree::Group(attr), TokenTree::Ident(kw), TokenTree::Ident(_), TokenTree::Group(body)] =
            window
        {
            if hash.as_char() == '#'
                && attr.delimiter() == Delimiter::Bracket
                && is_cfg_test(attr.stream())
                && kw == "mod"
                && body.delimiter() == Delimiter::Brace
            {
                out.push(body.stream());
            }
        }
    }
    out
}

/// `cfg(test)`, or `cfg(all(…))` with `test` among its predicates: the inside of the attribute.
fn is_cfg_test(attr: TokenStream) -> bool {
    let tokens: Vec<TokenTree> = attr.into_iter().collect();
    let is_test =
        |tokens: &[TokenTree]| matches!(tokens, [TokenTree::Ident(test)] if test == "test");
    match tokens.as_slice() {
        [TokenTree::Ident(cfg), TokenTree::Group(args)] if cfg == "cfg" => {
            let args: Vec<TokenTree> = args.stream().into_iter().collect();
            match args.as_slice() {
                [TokenTree::Ident(all), TokenTree::Group(predicates)] if all == "all" => {
                    let predicates: Vec<TokenTree> = predicates.stream().into_iter().collect();
                    predicates
                        .split(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ','))
                        .any(is_test)
                }
                args => is_test(args),
            }
        }
        _ => false,
    }
}

/// An identifier as rustc reads it: `r#name` is `name`.
fn plain(ident: &proc_macro2::Ident) -> String {
    let name = ident.to_string();
    name.strip_prefix("r#").map(str::to_string).unwrap_or(name)
}

/// Scan `tokens`, inside the function `within` (the nearest `fn` around them, if any).
fn walk(file: &str, within: Option<&Body>, tokens: TokenStream, out: &mut Vec<(usize, String)>) {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    // The function whose body the next brace group is: `fn <name>` seen, its body not yet.
    let mut next_fn: Option<String> = None;
    for (i, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Group(group) => {
                if group.delimiter() == Delimiter::Brace {
                    if let Some(name) = next_fn.take() {
                        let body = Body {
                            name,
                            sealed_first: starts_sealed(group.stream()),
                        };
                        walk(file, Some(&body), group.stream(), out);
                        continue;
                    }
                }
                walk(file, within, group.stream(), out);
            }
            // A declaration with no body (`fn f();`) leaves no body to wait for.
            TokenTree::Punct(p) if p.as_char() == ';' => next_fn = None,
            TokenTree::Ident(ident) => {
                if ident == "fn" {
                    if let Some(TokenTree::Ident(f)) = tokens.get(i + 1) {
                        next_fn = Some(plain(f));
                    }
                    continue;
                }
                let name = plain(ident);
                let builder = BUILDERS.contains(&name.as_str()) && !allowed(file, within, &name);
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

/// A function body being scanned: its function's name, and whether it starts with `sealed();`.
struct Body {
    name: String,
    sealed_first: bool,
}

/// Whether [`ALLOWED`] lets the function `within`, in `file`, name `builder`.
fn allowed(file: &str, within: Option<&Body>, builder: &str) -> bool {
    let Some(body) = within else {
        return false;
    };
    ALLOWED.iter().any(|&(f, function, b, sealed)| {
        f == file && function == body.name && b == builder && (!sealed || body.sealed_first)
    })
}

/// A body whose first statement is `sealed();`.
fn starts_sealed(body: TokenStream) -> bool {
    let tokens: Vec<TokenTree> = body.into_iter().take(3).collect();
    matches!(
        tokens.as_slice(),
        [TokenTree::Ident(f), TokenTree::Group(args), TokenTree::Punct(semi)]
            if f == "sealed"
                && args.delimiter() == Delimiter::Parenthesis
                && args.stream().is_empty()
                && semi.as_char() == ';'
    )
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
                && plain(ty) == handle
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
    // The scan saw the files it guards: the rig, the probes, and the app's own.
    for file in [
        "tests/common/mod.rs",
        "tests/common/plugins.rs",
        "tests/sealed_plugins.rs",
        "tests/single_instance.rs",
        "src/app.rs",
        "src/lib.rs",
        "src/single_instance.rs",
    ] {
        assert!(
            files.iter().any(|p| p.ends_with(file)),
            "{file} not scanned"
        );
    }
    // Each allowance names a function that exists where it says: one renamed away would leave
    // its builder unguarded under the new name, so it fails here instead.
    for (file, function, _, _) in ALLOWED {
        let src = fs::read_to_string(root.join(file)).unwrap();
        assert!(
            src.contains(&format!("fn {function}(")),
            "{file}: no fn {function}"
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
        // A raw identifier is the plain one to rustc.
        (
            "fn f() { r#tauri_plugin_opener::init(); }",
            "tauri_plugin_opener",
        ),
        (
            "fn f() { b.plugin(app::r#clipboard_plugin()); }",
            "clipboard_plugin",
        ),
        (
            "use r#tauri_plugin_clipboard_manager as c;",
            "tauri_plugin_clipboard_manager",
        ),
        // An allowed function's name does not carry into another file.
        (
            "fn opener_probe() { sealed(); b.plugin(app::opener_plugin()); }",
            "opener_plugin",
        ),
        // The single-instance plugin's builder is reached through its type.
        (
            "fn f() { SingleInstance::for_this_launch().register(b); }",
            "SingleInstance",
        ),
        (
            "use apprafter_desktop::single_instance::SingleInstance;",
            "SingleInstance",
        ),
        (
            "use apprafter_desktop::single_instance::SingleInstance as S;",
            "SingleInstance",
        ),
        (
            "fn wiring_probe() { SingleInstance::On.register(b); }",
            "SingleInstance",
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
        "fn f(a: &App) { a.try_state::<r#tauri_plugin_opener::Opener<MockRuntime>>(); }",
    ] {
        assert_eq!(found(src), Vec::<String>::new(), "{src}");
    }
    let none = |file: &str, src: &str| {
        assert_eq!(hits(file, src), Vec::new(), "{file}: {src}");
    };
    let some = |file: &str, src: &str| {
        assert!(!hits(file, src).is_empty(), "{file}: {src}");
    };
    // The app's own code may.
    none(
        "src/lib.rs",
        "fn f() { b.plugin(app::clipboard_plugin()); }",
    );
    none(
        "src/app.rs",
        "pub fn clipboard_plugin() -> P { tauri_plugin_clipboard_manager::init() }",
    );
    none(
        "src/single_instance.rs",
        "impl SingleInstance { pub fn register(&self, b: B) -> B { \
         b.plugin(tauri_plugin_single_instance::init(|_, _, _| {})) } }",
    );
    // Its test modules may not, but for the one allowance.
    let app_test = |body: &str| format!("fn app() {{}}\n#[cfg(test)]\nmod tests {{ {body} }}");
    some(
        "src/app.rs",
        &app_test("#[test] fn t() { builder(mock_builder()).plugin(clipboard_plugin()); }"),
    );
    some(
        "src/lib.rs",
        &app_test("fn t() { tauri_plugin_opener::init(); }"),
    );
    some(
        "src/app.rs",
        &app_test("fn the_opener_plugin_injects_no_script() { super::clipboard_plugin(); }"),
    );
    none(
        "src/app.rs",
        &app_test("fn the_opener_plugin_injects_no_script() { super::opener_plugin::<M>(); }"),
    );
    // A test module behind `all(test, …)` is a test module; one that is not only a test's is
    // the app's.
    let linux_test = |body: &str| {
        format!("fn app() {{}}\n#[cfg(all(target_os = \"linux\", test))]\nmod tests {{ {body} }}")
    };
    some(
        "src/single_instance.rs",
        &linux_test("fn t() { SingleInstance::On.register(mock_builder()); }"),
    );
    some(
        "src/single_instance.rs",
        &linux_test("fn t() { tauri_plugin_single_instance::init(|_, _, _| {}); }"),
    );
    none(
        "src/single_instance.rs",
        &linux_test(
            "fn an_address_that_does_not_parse_is_off_without_connecting() { \
             SingleInstance::on_session_bus(a, never); }",
        ),
    );
    none(
        "src/lib.rs",
        "#[cfg(any(test, windows))]\nmod m { fn f() { tauri_plugin_opener::init(); } }",
    );
    none(
        "src/lib.rs",
        "#[cfg(not(test))]\nmod m { fn f() { tauri_plugin_opener::init(); } }",
    );
    // The sealed probes may, each its own builder and only after `sealed();`.
    let sealed = "tests/sealed_plugins.rs";
    none(
        sealed,
        "fn opener_probe() { sealed(); b.plugin(app::opener_plugin()); }",
    );
    none(
        sealed,
        "fn clipboard_probe() { sealed(); let c = || b.plugin(app::clipboard_plugin()); }",
    );
    some(
        sealed,
        "fn opener_probe() { b.plugin(app::opener_plugin()); sealed(); }",
    );
    some(
        sealed,
        "fn opener_probe() { sealed(); b.plugin(app::clipboard_plugin()); }",
    );
    some(
        sealed,
        "fn another() { sealed(); b.plugin(app::opener_plugin()); }",
    );
    some(sealed, "fn f() { b.plugin(app::opener_plugin()); }");
    // A function nested in an allowed one is not the allowed one.
    some(
        sealed,
        "fn opener_probe() { sealed(); fn inner() { b.plugin(app::opener_plugin()); } }",
    );
    // The single-instance probe may name the lock's type, inside itself only.
    let single = "tests/single_instance.rs";
    none(
        single,
        "fn wiring_probe() { let s = SingleInstance::for_this_launch(); s.register(b); }",
    );
    some(
        single,
        "use apprafter_desktop::single_instance::SingleInstance;\nfn wiring_probe() {}",
    );
    some(
        single,
        "fn another() { SingleInstance::for_this_launch(); }",
    );
    some(
        single,
        "fn wiring_probe() { tauri_plugin_single_instance::init(|_, _, _| {}); }",
    );
}
