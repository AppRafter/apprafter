// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! WI-478 (d4a-h F1's class): every command that asks for a secret at its terminal refuses a
//! target store other accounts can change BEFORE it asks — never after a person typed a token,
//! a lock password or a passphrase into a command that could not have used it. Each case runs
//! the command at a pseudo-terminal of its own (`common::terminal`, which reads the terminal
//! while the command runs: GOTCHA-221), on a scratch store whose `targets` folder others may
//! write (mode 0777), with nothing typed: the command must end on its own, exit 1 with
//! `apprafter::secrets::store_not_private` naming the folder, and have drawn no prompt. A
//! command that asked instead would wait for an answer, and the conversation fails on its
//! deadline. Unix only: the Windows console has no pseudo-console in the runners; the spawned
//! `store_privacy_test` covers the refusals there.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use apprafter_core::secrets::testing::locked_store;
use common::spawn::Spawn;
use common::terminal::{converse, converse_at_terminal, Ended};

const TOKEN: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";

/// What a prompt of these commands shows, any one of which means it asked: the wizards'
/// questions (inquire's `? ` prefix), the token and passphrase prompts, the lock's prompts.
const PROMPTS: &[&str] = &[
    "Target name",
    "API token",
    "Rotating credentials",
    "Token",
    "passphrase",
    "Lock password",
    "Account password",
    "lock password",
    "recovery key",
    "Unlock the target store at",
    "is locked, and",
];

/// `prod` in a store in files, the CLI default, with `targets` then opened to other accounts.
fn files_store(sp: &Spawn) -> PathBuf {
    let out = sp
        .cmd(&[
            "target",
            "add",
            "prod",
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN,
            "--no-ping",
            "--no-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    loosen(sp)
}

/// `prod` in a locked store, the CLI default, its kubeconfig cached under the store's own key,
/// with `targets` then opened to other accounts.
fn locked(sp: &Spawn) -> PathBuf {
    let root = sp.store();
    let made = locked_store(&root, &[("prod", Some(TOKEN))]);
    let paths = cli_core::TargetStorePaths::for_root(root);
    cli_core::save_global_config(
        &paths,
        &cli_core::GlobalConfig {
            active_target: "prod".into(),
            version: cli_core::TARGET_STORE_VERSION,
        },
    )
    .unwrap();
    let state = cli_state::StatePaths::for_active_target(&paths, "prod");
    std::fs::create_dir_all(state.state_dir()).unwrap();
    let sealed = cli_core::secrets::encrypt_for_recipient(YAML, made.recipient()).unwrap();
    let body = serde_json::json!({
        "hetzner_cloud": {"server_id": 7, "server_name": "n", "kubeconfig_age": sealed},
    });
    std::fs::write(state.state_file(), body.to_string()).unwrap();
    loosen(sp)
}

/// Opens the store's `targets` folder to other accounts; the folder.
fn loosen(sp: &Spawn) -> PathBuf {
    let targets = sp.store().join("targets");
    std::fs::set_permissions(&targets, std::fs::Permissions::from_mode(0o777)).unwrap();
    targets
}

/// `kubectl` and `restic` stand-ins, which the repository and backup commands check for first.
fn with_tools(sp: Spawn) -> (Spawn, tempfile::TempDir) {
    let tools =
        common::stand_in::tool_stand_ins([&cli_core::tools::KUBECTL, &cli_core::tools::RESTIC]);
    (sp.with_path(tools.path().to_path_buf()), tools)
}

/// `text` without its terminal control sequences (`ESC [ … final byte`): inquire colours the
/// `?` of its prompts.
fn without_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn escapes_are_dropped_and_the_text_kept() {
    assert_eq!(
        without_escapes("\u{1b}[?2004h\u{1b}[38;5;10m?\u{1b}[39m Token:  "),
        "? Token:  "
    );
}

/// The command ended on its own with the refusal naming `folder`, and asked nothing.
fn refused_before_asking(end: &Ended, folder: &std::path::Path, args: &[&str]) {
    let text = &end.terminal;
    assert_eq!(end.status.code(), Some(1), "{args:?}:\n{text}");
    let unwrapped: String = text.replace('│', " ").split_whitespace().collect();
    let named: String = format!("other accounts can write to {}", folder.display())
        .split_whitespace()
        .collect();
    assert!(
        text.contains("apprafter::secrets::store_not_private") && unwrapped.contains(&named),
        "{args:?}:\n{text}"
    );
    let plain = without_escapes(text);
    for prompt in PROMPTS {
        assert!(
            !plain.contains(prompt),
            "{args:?} asked ({prompt:?}):\n{text}"
        );
    }
    assert!(
        !plain.lines().any(|l| l.trim_start().starts_with("? ")),
        "{args:?} drew a wizard prompt:\n{text}"
    );
    assert!(!text.contains(TOKEN), "{args:?}");
    assert!(end.restored, "{args:?}: the terminal was not put back");
}

/// `target add <new>` at a terminal runs the wizard, which asks for the token.
#[test]
fn target_add_refuses_before_its_wizard_asks_for_the_token() {
    let sp = Spawn::new();
    let folder = files_store(&sp);
    let args = ["target", "add", "dev", "--provider", "hetzner-cloud"];
    refused_before_asking(&converse_at_terminal(&sp, &args, &[]), &folder, &args);
}

/// `target add <t> --renew` with no `--token`: the renewal wizard's masked token prompt; and
/// with no name, its question for the target first.
#[test]
fn a_renewal_refuses_before_its_wizard_asks_for_the_new_token() {
    let sp = Spawn::new();
    let folder = files_store(&sp);
    for args in [
        &["target", "add", "prod", "--renew"][..],
        &["target", "add", "--renew"],
    ] {
        refused_before_asking(&converse_at_terminal(&sp, args, &[]), &folder, args);
    }
}

/// `lock on` asks for a new lock password (d4a-h F1, here at a terminal).
#[test]
fn lock_on_refuses_before_it_asks_for_a_password() {
    let sp = Spawn::new();
    let folder = files_store(&sp);
    let args = ["lock", "on", "--lock-password"];
    refused_before_asking(&converse(&sp, &args, &[]), &folder, &args);
}

/// D.4b-e1a: `lock on`'s default asks for the account password, or a lock password where it
/// falls back — refused all the same before it asks.
#[test]
fn lock_on_s_default_refuses_before_it_asks_for_a_password() {
    let sp = Spawn::new();
    let folder = files_store(&sp);
    let args = ["lock", "on"];
    refused_before_asking(&converse(&sp, &args, &[]), &folder, &args);
}

/// `lock off` on a locked store asks for its lock password or recovery key (d4a-h F1).
#[test]
fn lock_off_refuses_before_it_asks_for_the_lock_password() {
    let sp = Spawn::new();
    let folder = locked(&sp);
    let args = ["lock", "off"];
    refused_before_asking(&converse(&sp, &args, &[]), &folder, &args);
}

/// `unlock` asks for the lock password or the recovery key, then opens a window for a vault
/// whose every secret would be refused.
#[test]
fn unlock_refuses_before_it_asks_for_the_lock_password() {
    let sp = Spawn::new();
    let folder = locked(&sp);
    let args = ["unlock"];
    refused_before_asking(&converse(&sp, &args, &[]), &folder, &args);
}

/// A command that needs a locked store's secret asks for its lock password inline
/// (`vault_access`); the secret store refuses the store before it opens a session.
#[test]
fn a_command_on_a_locked_store_refuses_before_its_inline_prompt() {
    let sp = Spawn::new();
    let folder = locked(&sp);
    let args = ["kubeconfig"];
    let end = converse(&sp, &args, &[]);
    refused_before_asking(&end, &folder, &args);
    assert!(end.stdout.is_empty());
}

/// `repo creds add` asks for the repository token — masked, or in its wizard at a terminal —
/// before it reads the cluster's kubeconfig from the store.
#[test]
fn repo_creds_add_refuses_before_it_asks_for_the_token() {
    let (sp, _tools) = with_tools(Spawn::new());
    let folder = files_store(&sp);
    let args = [
        "repo",
        "creds",
        "add",
        "gh",
        "--url-prefix",
        "https://github.com/acme",
    ];
    refused_before_asking(&converse(&sp, &args, &[]), &folder, &args);
    refused_before_asking(&converse_at_terminal(&sp, &args, &[]), &folder, &args);
}

/// `repo creds rotate` reads the kubeconfig before it asks for the new token: refused there.
#[test]
fn repo_creds_rotate_refuses_before_it_asks_for_the_token() {
    let (sp, _tools) = with_tools(Spawn::new());
    let folder = files_store(&sp);
    let paths = cli_core::TargetStorePaths::for_root(sp.store());
    let state = cli_state::StatePaths::for_active_target(&paths, "prod");
    std::fs::create_dir_all(state.state_dir()).unwrap();
    let body = serde_json::json!({
        "hetzner_cloud": {"server_id": 7, "server_name": "n", "kubeconfig_yaml": YAML},
    });
    std::fs::write(state.state_file(), body.to_string()).unwrap();
    let args = ["repo", "creds", "rotate", "gh"];
    refused_before_asking(&converse(&sp, &args, &[]), &folder, &args);
}

/// `backup create` asks for the repository's passphrase before it reads the kubeconfig.
#[test]
fn backup_create_refuses_before_it_asks_for_the_passphrase() {
    let (sp, _tools) = with_tools(Spawn::new());
    let folder = files_store(&sp);
    let args = ["backup", "create"];
    refused_before_asking(&converse(&sp, &args, &[]), &folder, &args);
}

/// `restore` asks for the repository's passphrase before it reads the target's kubeconfig.
#[test]
fn restore_refuses_before_it_asks_for_the_passphrase() {
    let (sp, _tools) = with_tools(Spawn::new());
    let folder = files_store(&sp);
    let repo = sp.path("repo").display().to_string();
    let args = ["restore", repo.as_str()];
    refused_before_asking(&converse(&sp, &args, &[]), &folder, &args);
}
