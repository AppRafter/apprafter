// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! D.4a T46 (critic S5): every long-running command drops the store's data key as soon as it
//! holds what it needs — the statement right after its kubeconfig (or `up`'s phase 0) is
//! `vault_access::end_secret_phase()`. Read as tokens (proc-macro2): a function is found among
//! its file's top-level items, its body split at top-level `;`.

use std::fs;
use std::path::Path;

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// (file under `platform-cli/`, function, the call after which it holds what it needs).
const LONG_RUNNING: &[(&str, &str, &str)] = &[
    ("src/commands/top.rs", "run", "ensure_kubeconfig_tempfile"),
    (
        "src/commands/open.rs",
        "argocd",
        "ensure_kubeconfig_tempfile",
    ),
    (
        "src/commands/app_open.rs",
        "open",
        "ensure_kubeconfig_tempfile",
    ),
    ("src/commands/app.rs", "logs", "ensure_kubeconfig_tempfile"),
    (
        "src/commands/backup.rs",
        "run_export",
        "ensure_kubeconfig_tempfile",
    ),
    (
        "src/commands/backup.rs",
        "run_backup",
        "ensure_kubeconfig_tempfile",
    ),
    (
        "src/commands/backup.rs",
        "run_backup_trigger",
        "ensure_kubeconfig_tempfile",
    ),
    (
        "src/commands/bootstrap_all.rs",
        "run",
        "check_before_provisioning",
    ),
    (
        "src/commands/restore.rs",
        "run_restore",
        "ensure_kubeconfig_tempfile_for_target",
    ),
];

/// The body of the top-level `fn name` in `src`.
fn body_of(src: &str, name: &str) -> Vec<TokenTree> {
    let tokens: Vec<TokenTree> = src
        .parse::<TokenStream>()
        .expect("the file lexes")
        .into_iter()
        .collect();
    for (i, t) in tokens.iter().enumerate() {
        let is_fn = matches!(t, TokenTree::Ident(id) if id == "fn");
        let named = matches!(tokens.get(i + 1), Some(TokenTree::Ident(n)) if n == name);
        if is_fn && named {
            let body = tokens[i + 2..].iter().find_map(|t| match t {
                TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => Some(g),
                _ => None,
            });
            return body.expect("a body").stream().into_iter().collect();
        }
    }
    panic!("no top-level `fn {name}`")
}

/// `body` split at its top-level `;`.
fn statements(body: &[TokenTree]) -> Vec<Vec<TokenTree>> {
    let mut out = vec![Vec::new()];
    for t in body {
        if matches!(t, TokenTree::Punct(p) if p.as_char() == ';') {
            out.push(Vec::new());
        } else {
            out.last_mut().expect("one").push(t.clone());
        }
    }
    out
}

fn mentions(tokens: &[TokenTree], ident: &str) -> bool {
    tokens.iter().any(|t| match t {
        TokenTree::Ident(i) => i == ident,
        TokenTree::Group(g) => mentions(&g.stream().into_iter().collect::<Vec<_>>(), ident),
        _ => false,
    })
}

/// The listed functions' violations: `file fn` where the statement after the need does not
/// end the secret phase.
fn violations(read: impl Fn(&str) -> String) -> Vec<String> {
    let mut out = Vec::new();
    for (file, func, need) in LONG_RUNNING {
        let stmts = statements(&body_of(&read(file), func));
        let at = stmts
            .iter()
            .position(|s| mentions(s, need))
            .unwrap_or_else(|| {
                panic!("{file} `{func}` no longer calls `{need}`: update LONG_RUNNING")
            });
        if !stmts
            .get(at + 1)
            .is_some_and(|s| mentions(s, "end_secret_phase"))
        {
            out.push(format!("{file} `{func}`"));
        }
    }
    out
}

#[test]
fn every_long_running_command_ends_its_secret_phase_once_it_holds_what_it_needs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let missing = violations(|file| fs::read_to_string(root.join(file)).unwrap());
    assert!(
        missing.is_empty(),
        "the statement after the kubeconfig (or up's phase 0) must be \
         `crate::vault_access::end_secret_phase();` (D.4a T46): {missing:?}"
    );
}

/// The scan finds the call only in the statement right after the need, in the named
/// top-level function, and not in a comment.
#[test]
fn the_scan_wants_the_call_right_after_the_need() {
    let good = "fn run() { let kc = ensure_kubeconfig_tempfile()?; crate::vault_access::end_secret_phase(); go(); }";
    let late = "fn run() { let kc = ensure_kubeconfig_tempfile()?; go(); end_secret_phase(); }";
    let commented =
        "fn run() { let kc = ensure_kubeconfig_tempfile()?;\n// end_secret_phase();\ngo(); }";
    let stmts = |src: &str| statements(&body_of(src, "run"));
    let ok = |src: &str| {
        let s = stmts(src);
        let at = s
            .iter()
            .position(|x| mentions(x, "ensure_kubeconfig_tempfile"))
            .unwrap();
        s.get(at + 1)
            .is_some_and(|x| mentions(x, "end_secret_phase"))
    };
    assert!(ok(good));
    assert!(!ok(late));
    assert!(!ok(commented));
}
