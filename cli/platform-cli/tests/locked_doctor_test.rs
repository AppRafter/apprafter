// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter doctor` on a locked target store (D.4a T66, T71–T73, O1): with no terminal and no
//! unlock window it asks nothing, skips what needs a secret — saying how to unlock once — and
//! lists what the lock does not cover, never showing a secret. No golden: the machine-limit rows
//! depend on the host running the suite (Yama, the OS), so these tests assert the lines that do
//! not (as `lock status`'s tests do).
#![cfg(unix)]

mod common;

use apprafter_core::secrets::testing::locked_store;
use common::golden::{Sandbox, TOKEN_A};

/// The shape of an unencrypted OpenSSH private key (all zeros: no usable key), where the
/// kubeconfig fetch looks first — `~/.ssh/id_ed25519` in the sandbox's home.
const UNENCRYPTED_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACAREREREREREREREREREREREREREREREREREREREREREQAAABAAAAAAAAAA
AAAAAAAAAAAA
-----END OPENSSH PRIVATE KEY-----
";

const SEALED_STUB: &str =
    "-----BEGIN AGE ENCRYPTED FILE-----\\nx\\n-----END AGE ENCRYPTED FILE-----\\n";

/// `prod` in a locked store, the CLI default, provisioned with a cached kubeconfig.
fn locked(sb: &Sandbox) {
    locked_store(&sb.path("apprafter-config"), &[("prod", Some(TOKEN_A))]);
    sb.seed_pointer("prod");
    sb.seed_state(
        "prod",
        &format!(r#"{{"hetzner_cloud":{{"server_id":7,"server_name":"n","kubeconfig_age":"{SEALED_STUB}"}}}}"#),
    );
}

fn run(sb: &Sandbox, args: &[&str]) -> (Option<i32>, String, String) {
    let out = sb.std_cmd(args).output().expect("run apprafter");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// T66, O1: no terminal, no window — the rows that need a secret are skipped, the unlock hint
/// said once; the lock's own rows say what it does not cover; no secret, no vault write.
#[test]
fn doctor_on_a_locked_store_without_a_terminal_asks_nothing_and_says_what_it_skipped() {
    let sb = Sandbox::new().with_stand_in_tools();
    locked(&sb);
    std::fs::create_dir_all(sb.path("home/.ssh")).unwrap();
    std::fs::write(sb.path("home/.ssh/id_ed25519"), UNENCRYPTED_KEY).unwrap();
    let vault = std::fs::read(sb.path("apprafter-config/vault")).unwrap();
    let (code, stdout, stderr) = run(&sb, &["doctor"]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    for line in [
        "  ✓ Secrets locked (1 slot: recovery key) (not run from a terminal)\n",
        "  – Token format valid (the target store is locked)\n      hint: run `apprafter unlock`, then `apprafter doctor` again for the token and cluster checks\n",
        "  ⚠ The SSH key ~/.ssh/id_ed25519 that reaches your nodes has no passphrase\n",
        "  ℹ While a command talks to a cluster, its kubeconfig is a file programs running as you can read\n",
        "  ℹ A program running as you that sets out to get past the lock can read what you type, change what you run or use an unlocked window; run AI agents under a separate OS account or in a sandbox\n",
        "  ℹ Hibernation writes memory to disk, an open window's key included\n",
    ] {
        assert!(stdout.contains(line), "missing {line:?}:\n{stdout}");
    }
    assert_eq!(
        stdout.matches("(the target store is locked)").count(),
        4,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("hint: run `apprafter unlock`").count(),
        1,
        "{stdout}"
    );
    assert!(!stdout.contains(TOKEN_A) && !stderr.contains(TOKEN_A));
    assert_eq!(
        std::fs::read(sb.path("apprafter-config/vault")).unwrap(),
        vault,
        "doctor writes no vault"
    );
}

/// T35, §3.13: HCLOUD_TOKEN set beside a locked store is a warning naming it — never its value.
#[test]
fn hcloud_token_beside_a_locked_store_is_named_never_shown() {
    let env_token = "e".repeat(64);
    let sb = Sandbox::new()
        .with_stand_in_tools()
        .with_env("HCLOUD_TOKEN", &env_token);
    locked(&sb);
    let (_, stdout, stderr) = run(&sb, &["doctor", "--no-ping"]);
    assert!(
        stdout.contains("  ⚠ HCLOUD_TOKEN is set, and this locked store refuses it\n"),
        "{stdout}"
    );
    assert!(!stdout.contains(&env_token) && !stderr.contains(&env_token));
}
