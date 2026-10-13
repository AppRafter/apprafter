// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `APPRAFTER_SANDBOX` (D.4a T5, `src/sandbox.rs`; safety-net mutants 10–13, critic C4 and W1).
//! With it set, the CLI refuses to run when a location it would use resolves outside the
//! sandbox. Each row builds its own escape — a variable pointing outside, a removed `HOME`, a
//! symlink out of the sandbox — and runs `target list`, which reads only the store, so even a
//! fence that let it through would touch nothing real. A refusal is exit 1 with
//! `apprafter::sandbox::escape` naming the location, and nothing written outside. The other
//! rows show the fence admits what it should and changes nothing unless set.

mod common;

use std::path::Path;
use std::process::Output;

use common::spawn::{Spawn, SANDBOX_ENV};

/// stderr with miette's wrapping undone: its `│` gutter dropped, each run of whitespace one space.
fn squeeze(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn list(sp: &Spawn) -> Output {
    sp.cmd(&["target", "list"]).output().expect("run apprafter")
}

/// `target list` in `sp` is refused, naming `what`, and `outside` stays empty.
fn refused(sp: &Spawn, what: &str, outside: &Path) {
    let out = list(sp);
    let stderr = squeeze(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("apprafter::sandbox::escape"), "{stderr}");
    assert!(
        stderr.contains(what),
        "the refusal must name {what}: {stderr}"
    );
    assert_eq!(
        std::fs::read_dir(outside).unwrap().count(),
        0,
        "a refused run wrote into {}",
        outside.display()
    );
}

/// A directory outside every sandbox root.
fn outside() -> tempfile::TempDir {
    tempfile::tempdir().expect("an outside directory")
}

#[test]
fn inside_the_sandbox_every_command_runs() {
    let store = tempfile::tempdir().unwrap();
    for sp in [Spawn::new(), Spawn::new().with_store(store.path())] {
        let out = list(&sp);
        assert!(out.status.success(), "{}", squeeze(&out.stderr));
        assert!(!squeeze(&out.stderr).contains("apprafter::sandbox"));
    }
}

#[test]
fn without_the_variable_nothing_is_fenced() {
    let out_dir = outside();
    let sp = Spawn::new()
        .with_env("APPRAFTER_CONFIG_DIR", out_dir.path())
        .without_env(SANDBOX_ENV);
    let out = list(&sp);
    assert!(out.status.success(), "{}", squeeze(&out.stderr));
}

/// Mutant 10 (`enforce` returns `Ok`) fails every refusal row; mutant 11 (one location forgotten)
/// the row of that location, which then runs or names another.
#[test]
fn a_store_outside_is_refused() {
    let out = outside();
    refused(
        &Spawn::new().with_env("APPRAFTER_CONFIG_DIR", out.path().join("store")),
        "the target store (APPRAFTER_CONFIG_DIR)",
        out.path(),
    );
}

/// Critic W1: once the variable is set, `HOME` lies inside it.
#[cfg(unix)]
#[test]
fn a_home_outside_is_refused() {
    let out = outside();
    refused(
        &Spawn::new().with_env("HOME", out.path()),
        "the home directory (HOME)",
        out.path(),
    );
}

/// Without `HOME` the password database answers — with the real home.
#[cfg(unix)]
#[test]
fn a_removed_home_is_refused() {
    let out = list(&Spawn::new().without_env("HOME"));
    let stderr = squeeze(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("apprafter::sandbox::escape")
            && stderr.contains("the home directory (HOME)"),
        "{stderr}"
    );
}

/// With no `APPRAFTER_CONFIG_DIR`, the store comes from `XDG_CONFIG_HOME` on Linux (macOS's
/// config directory ignores it).
#[cfg(target_os = "linux")]
#[test]
fn an_xdg_config_home_outside_is_refused() {
    let out = outside();
    let sp = Spawn::new()
        .without_env("APPRAFTER_CONFIG_DIR")
        .with_env("XDG_CONFIG_HOME", out.path());
    refused(&sp, "the target store (APPRAFTER_CONFIG_DIR)", out.path());
}

/// Critic C4: in d4a-a the age key is fenced for every command (d4a-c skips it only where a
/// locked store refuses the override, never for `lock on` / `lock off`).
#[test]
fn an_age_key_outside_is_refused() {
    let out = outside();
    refused(
        &Spawn::new().with_env("APPRAFTER_AGE_KEY", out.path().join("age.key")),
        "the age key (APPRAFTER_AGE_KEY)",
        out.path(),
    );
}

/// T5: on a locked store the age key is not fenced for a command other than `lock on` /
/// `lock off` — the variable is refused there and the shared key is never read (T35); a Windows
/// test that omits `APPRAFTER_AGE_KEY` resolves the real profile's key, which must not stop it.
#[test]
fn an_age_key_outside_is_not_fenced_on_a_locked_store() {
    let out = outside();
    let sp = Spawn::new().with_env("APPRAFTER_AGE_KEY", out.path().join("age.key"));
    apprafter_core::secrets::testing::locked_store(&sp.store(), &[("prod", None)]);
    let run = list(&sp);
    assert!(run.status.success(), "{}", squeeze(&run.stderr));
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}

#[test]
fn an_ssh_identity_outside_is_refused() {
    let out = outside();
    refused(
        &Spawn::new().with_env("APPRAFTER_SSH_PRIVATE_KEY", out.path().join("id_ed25519")),
        "the SSH identity (APPRAFTER_SSH_PRIVATE_KEY)",
        out.path(),
    );
}

/// Mutant 12 (paths compared as written): the runtime directory is a symlink out of the sandbox.
#[cfg(unix)]
#[test]
fn a_runtime_dir_that_links_outside_is_refused() {
    let out = outside();
    let sp = Spawn::new();
    std::os::unix::fs::symlink(out.path(), sp.store().join("run")).unwrap();
    refused(&sp, "the runtime directory", out.path());
}

#[cfg(unix)]
#[test]
fn a_store_that_links_outside_is_refused() {
    let out = outside();
    let sp = Spawn::new();
    let link = sp.path("linked-store");
    std::os::unix::fs::symlink(out.path(), &link).unwrap();
    refused(
        &sp.with_env("APPRAFTER_CONFIG_DIR", &link),
        "the target store (APPRAFTER_CONFIG_DIR)",
        out.path(),
    );
}

/// D.4a c2: `store.yaml` and the vault are fenced too: a symlink from either out of the sandbox
/// is refused before anything reads it.
#[cfg(unix)]
#[test]
fn a_lock_state_that_links_outside_is_refused() {
    let out = outside();
    let sp = Spawn::new();
    std::fs::create_dir_all(sp.store()).unwrap();
    std::os::unix::fs::symlink(out.path(), sp.store().join("store.yaml")).unwrap();
    refused(
        &sp,
        "the target store's lock state (<target store>/store.yaml)",
        out.path(),
    );
}

#[cfg(unix)]
#[test]
fn a_vault_that_links_outside_is_refused() {
    let out = outside();
    let sp = Spawn::new();
    std::fs::create_dir_all(sp.store()).unwrap();
    std::os::unix::fs::symlink(out.path(), sp.store().join("vault")).unwrap();
    refused(
        &sp,
        "the target store's vault (<target store>/vault)",
        out.path(),
    );
}

#[test]
fn a_root_that_is_relative_or_missing_is_refused() {
    let gone = Spawn::new().path("not-there");
    for root in [
        std::ffi::OsString::new(),
        "relative/root".into(),
        gone.into_os_string(),
    ] {
        let out = list(&Spawn::new().with_env(SANDBOX_ENV, &root));
        let stderr = squeeze(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{root:?}: {stderr}");
        assert!(
            stderr.contains("apprafter::sandbox::bad_root"),
            "{root:?}: {stderr}"
        );
    }
    // A relative root that EXISTS from the child's working directory (the sandbox root, where
    // `home` is a directory): only the absolute-path check refuses it. Without that check the
    // fence would accept `home` and then refuse the store (`<root>/apprafter-config`, outside
    // it) as an escape — a different code, so this row fails.
    let sp = Spawn::new();
    let root = sp.root().to_path_buf();
    let out = list(&sp.current_dir(&root).with_env(SANDBOX_ENV, "home"));
    let stderr = squeeze(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "\"home\": {stderr}");
    assert!(
        stderr.contains("apprafter::sandbox::bad_root"),
        "\"home\": {stderr}"
    );
}
