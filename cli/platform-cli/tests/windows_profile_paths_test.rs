// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Release gate for the Windows CLI (desktop design spec §6.5): on Windows the
//! CLI's per-user defaults live under the signed-in user's profile and roaming
//! application-data folder, whatever `HOME` says.
//!
//! Git Bash, MSYS2 and Cygwin export a `HOME` of their own, and a native
//! Windows program inherits it. The CLI asks Windows for its Known Folders
//! (through `dirs`) instead, so with `HOME` pointing anywhere:
//! - the age key is `%USERPROFILE%\.config\apprafter\age.key`;
//! - the SSH identity for node round-trips is `%USERPROFILE%\.ssh\id_ed25519`;
//! - the home directory the shared core reads (the add wizard's SSH key default
//!   and candidate list are built from it) is `%USERPROFILE%`;
//! - the target store is `%APPDATA%\apprafter`, with the core's runtime
//!   directory under it.
//!
//! `USERPROFILE` and `APPDATA` serve as the independent witness: the code under
//! test never reads them, so a regression that starts trusting an environment
//! variable shows up as a path somewhere else. A profile path with a space
//! cannot be produced on a CI runner; the hand test on Windows covers it.
//!
//! One test, because it changes this process's environment, in a file of its
//! own, so nothing else runs in its test binary.
#![cfg(windows)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use apprafter_core::{Context, EnvSource};

/// The process environment, read as the CLI's own `EnvSource` reads it
/// (`platform-cli/src/context.rs`, crate-private). The core is handed the very
/// environment whose `HOME` is wrong, so a context builder that started
/// reading `HOME` through its `EnvSource` would show up here too.
struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn var_os(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }
}

/// How Windows compares paths: case-insensitively, with either separator.
fn norm(p: &Path) -> String {
    p.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn assert_same_path(what: &str, got: &Path, want: &Path) {
    assert_eq!(norm(got), norm(want), "{what}: got {got:?}, want {want:?}");
}

fn known_folder(var: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(var).unwrap_or_else(|| panic!("{var} is set in every Windows session")),
    );
    assert!(path.is_absolute(), "{var}={path:?} is not absolute");
    path
}

#[test]
fn per_user_defaults_resolve_under_the_profile_whatever_home_says() {
    let profile = known_folder("USERPROFILE");
    let appdata = known_folder("APPDATA");

    // What Git Bash does, made deliberately wrong: a HOME outside the profile.
    // Every override the defaults honour is removed, so only defaults answer.
    let not_the_profile = PathBuf::from(r"C:\apprafter-not-the-profile");
    std::env::set_var("HOME", &not_the_profile);
    std::env::set_var("XDG_CONFIG_HOME", not_the_profile.join("xdg"));
    for var in [
        "APPRAFTER_AGE_KEY",
        "APPRAFTER_SSH_PRIVATE_KEY",
        "APPRAFTER_CONFIG_DIR",
    ] {
        std::env::remove_var(var);
    }

    let age_key = profile.join(".config").join("apprafter").join("age.key");
    let ssh_identity = profile.join(".ssh").join("id_ed25519");
    let store = appdata.join("apprafter");

    // The lower crates, which every command the core does not own yet calls.
    assert_same_path(
        "cli_core::secrets::default_age_key_path",
        &cli_core::secrets::default_age_key_path(),
        &age_key,
    );
    assert_same_path(
        "cli_providers::hetzner_cloud::default_ssh_identity_path",
        &cli_providers::hetzner_cloud::default_ssh_identity_path(),
        &ssh_identity,
    );
    assert_same_path(
        "cli_core::target::default_config_root",
        &cli_core::target::default_config_root().expect("a config root"),
        &store,
    );
    assert_same_path(
        "cli_core::paths::home_dir",
        &cli_core::paths::home_dir().expect("a home directory"),
        &profile,
    );

    // The core, built as the target, doctor and whoami commands build it: from
    // the process environment, which now carries the wrong HOME and no override.
    let ctx = Context::from_cli_env(&ProcessEnv).expect("a CLI context");
    assert_same_path(
        "Context::home_dir",
        ctx.home_dir().expect("a home"),
        &profile,
    );
    assert_same_path("Context::age_key_path", ctx.age_key_path(), &age_key);
    assert_same_path("Context::config_root", ctx.config_root(), &store);
    assert_same_path(
        "Context::runtime_dir",
        ctx.runtime_dir(),
        &store.join("run"),
    );
}
