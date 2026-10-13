// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! T-lazy-1 (D.4a T7, G5; ADR 0068 Risks "a test could reach a real store or holder"): the
//! commands that need no secret read no age key. The same steps run in two sandboxes that differ
//! in one thing — in the second, the age key's path is a DIRECTORY, so any read of it fails
//! (`EISDIR`, which holds even for root, unlike a mode-000 file) — and every step must exit and
//! print byte for byte the same in both. The control proves the trap is armed: `kubeconfig`
//! over a cached, encrypted kubeconfig reads the key, and its output differs.
//!
//! What this cannot see: a read whose error the command swallows. D.4a's in-process key counter
//! (T-lazy-3, with the vault) closes that.

mod common;

use std::ffi::OsString;
use std::fs;

use common::mask::{mask_millis, mask_timestamps};
use common::spawn::Spawn;

/// 64 ASCII alphanumerics: the shape `cli_core::target` accepts.
const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The commands of T7 that need no secret, in order: each sees the store the ones before it
/// left. (`doctor` reads the key, so it is not one; `target add` / `rename` / `remove` will write
/// a locked store's vault, so they are not either.)
const NO_SECRET: &[&[&str]] = &[
    &["--help"],
    &["--version"],
    &["completion", "bash"],
    &["target", "list"],
    &["target", "show", "prod"],
    &["target", "use", "prod"],
    &["plan"],
    &[
        "app",
        "scaffold",
        "--runtime",
        "blank",
        "--name",
        "lazy",
        "--path",
        "scaffold",
    ],
    &["app", "validate", "scaffold/apprafter"],
];

/// A `cue` for `app validate`: `CUE_BIN` as the test runs with it, else the first `cue` on its
/// `PATH`. Building these tests needed one (cli-providers' build script runs it), so a missing
/// one is a failure, not a skip.
fn cue() -> OsString {
    std::env::var_os("CUE_BIN")
        .filter(|v| !v.is_empty())
        .or_else(|| {
            let path = std::env::var_os("PATH").unwrap_or_default();
            cli_core::tools::find_on_path("cue", &path, cli_core::tools::is_executable_file)
                .map(|p| p.into_os_string())
        })
        .expect("`app validate` needs cue: set CUE_BIN or put cue on PATH, as the CLI build does")
}

/// A kubeconfig encrypted to a key neither sandbox holds. Made once and cached in both: age
/// encryption is randomised, and `plan` logs the state it read, ciphertext included, so two
/// encryptions would make the sandboxes differ in more than the trap.
fn kubeconfig_for_a_key_elsewhere() -> String {
    let elsewhere = age::x25519::Identity::generate();
    cli_core::secrets::encrypt_for_recipient("apiVersion: v1\n", &elsewhere.to_public()).unwrap()
}

/// A sandbox with target `prod` (no ping), whose state caches `armored`: enough for `target
/// show` to show it and for `kubeconfig` to read the key. No age key exists afterwards.
fn sandbox(armored: &str) -> Spawn {
    let sp = Spawn::new().with_env("CUE_BIN", cue());
    let ssh_key = sp.home().join("id_ed25519.pub");
    fs::write(&ssh_key, "ssh-ed25519 AAAA laziness test key\n").unwrap();
    let added = sp
        .cmd(&[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN,
            "--ssh-key",
            ssh_key.to_str().unwrap(),
            "--region",
            "nbg1",
            "--tier",
            "solo",
            "--no-ping",
            "--no-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let state = sp.store().join("state").join("prod").join(".apprafter");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("state.json"),
        serde_json::json!({"hetzner_cloud": {
            "server_id": 1, "server_name": "prod-node", "kubeconfig_age": armored,
        }})
        .to_string(),
    )
    .unwrap();
    assert!(
        !sp.age_key().exists(),
        "setting the sandbox up made an age key; T-lazy-1 needs it absent"
    );
    sp
}

/// One step's exit and output, normalised so two runs compare: each sandbox's root masked, then
/// tracing's timestamps (`<TS>`) and timings (`<MS> ms`), as the golden harness masks them.
fn run(sp: &Spawn, args: &[&str]) -> (Option<i32>, String, String) {
    let out = sp.cmd(args).output().expect("run apprafter");
    let norm = |bytes: &[u8]| {
        mask_millis(&mask_timestamps(
            &sp.mask_root(&String::from_utf8_lossy(bytes)),
        ))
    };
    (out.status.code(), norm(&out.stdout), norm(&out.stderr))
}

#[test]
fn commands_that_need_no_secret_never_read_the_age_key() {
    let armored = kubeconfig_for_a_key_elsewhere();
    let plain = sandbox(&armored);
    let trapped = sandbox(&armored);
    fs::create_dir_all(trapped.age_key()).unwrap();
    assert!(
        fs::read_to_string(trapped.age_key()).is_err(),
        "the trap must make a read of the key fail"
    );

    // The normaliser first: one step run twice in one sandbox compares equal, so whatever
    // differs below differs between the sandboxes, not between two runs (an unmasked tracing
    // timestamp would make every comparison fail, and the control pass, for no reason). Run in
    // both sandboxes, so their histories stay the same.
    for sp in [&plain, &trapped] {
        assert_eq!(
            run(sp, &["target", "list"]),
            run(sp, &["target", "list"]),
            "two runs of one step in one sandbox differ after run() normalised them: a per-run \
             byte run() does not mask"
        );
    }

    // The control: a command that reads the key shows the trap.
    assert_ne!(
        run(&plain, &["kubeconfig"]),
        run(&trapped, &["kubeconfig"]),
        "kubeconfig reads the age key, so the trap must show in its output; it did not, and this \
         test could not see a read"
    );

    for args in NO_SECRET {
        assert_eq!(
            run(&plain, args),
            run(&trapped, args),
            "`apprafter {}` behaves differently when the age key cannot be read: it read the key",
            args.join(" ")
        );
    }
    assert!(trapped.age_key().is_dir(), "a command replaced the trap");
}
