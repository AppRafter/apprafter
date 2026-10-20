// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter lock on` / `lock off` as a person and a script meet them (D.4a T36, T37, T60;
//! O2, O12, O13; L5). The conversations run at a pseudo-terminal of their own
//! (`common::terminal`, Unix); the refusals without a terminal on every OS. A plain build
//! derives release keys (seconds per `lock on`), so the cases that create or open a password
//! slot run one at a time; a feature build (`insecure-fast-kdf`) derives weak ones, and only
//! because the store lies inside `APPRAFTER_SANDBOX`.

mod common;

#[cfg(unix)]
use std::sync::{Mutex, MutexGuard, PoisonError};

use apprafter_core::secrets::testing::locked_store;
use apprafter_core::vault::KdfPolicy;
use common::spawn::Spawn;

const TOKEN: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
#[cfg(unix)]
const PASSWORD: &str = "correct horse battery staple";

/// One password-slot case at a time: a release derivation takes 256 MiB.
#[cfg(unix)]
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `prod` (TOKEN), the CLI default, in a store in files.
fn seeded(sp: &Spawn) {
    let store = cli_core::TargetStorePaths::for_root(sp.store());
    cli_core::save_target(
        &store,
        &cli_core::Target {
            name: "prod".into(),
            config: cli_core::TargetConfig {
                provider: "hetzner-cloud".into(),
                region: Some("nbg1".into()),
                ..Default::default()
            },
            credentials: cli_core::TargetCredentials {
                hetzner_token: Some(TOKEN.into()),
            },
        },
    )
    .unwrap();
    cli_core::save_global_config(
        &store,
        &cli_core::GlobalConfig {
            active_target: "prod".into(),
            version: cli_core::TARGET_STORE_VERSION,
        },
    )
    .unwrap();
}

fn run(sp: &Spawn, args: &[&str]) -> (Option<i32>, String, String) {
    let out = sp.cmd(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn store_yaml(sp: &Spawn) -> Option<String> {
    std::fs::read_to_string(sp.store().join("store.yaml")).ok()
}

/// D.4b B55, B88 (1): what a plain `lock on` says first where APPRAFTER_SANDBOX fences this
/// account on Linux and macOS — every spawned test's (`src/commands/lock.rs`'s
/// `fallback_head`).
const SANDBOX_HEAD_UNIX: &str = "AppRafter does not check your account password while \
                                 APPRAFTER_SANDBOX is set: the variable fences tests and \
                                 agents away from it.\nThe store gets a separate lock \
                                 password instead.\n";

/// Every file under `dir` holding `needle`.
#[cfg(unix)]
fn holding(dir: &std::path::Path, needle: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(holding(&path, needle));
        } else if String::from_utf8_lossy(&std::fs::read(&path).unwrap()).contains(needle) {
            out.push(path);
        }
    }
    out
}

/// C38: `lock on` asks a person for a new password; no terminal, no lock, nothing written.
#[test]
fn lock_on_without_a_terminal_is_refused() {
    let sp = Spawn::new();
    seeded(&sp);
    let (code, _, stderr) = run(&sp, &["lock", "on", "--lock-password"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("apprafter::cli::usage_refused"), "{stderr}");
    assert!(
        stderr.contains("asks for a new lock password and shows a recovery key"),
        "{stderr}"
    );
    assert_eq!(store_yaml(&sp), None);
}

/// D.4b B55, F4 (d4b-e1a): a plain `lock on` with no terminal, where APPRAFTER_SANDBOX fences
/// this account, says why it would take a lock password — the fence's two lines, naming
/// Windows Hello on Windows (`lock_on_fallback_sandbox_windows`) — then refuses as D.4a did;
/// nothing is written.
#[test]
fn a_plain_lock_on_without_a_terminal_says_why_it_takes_a_lock_password() {
    let sp = Spawn::new();
    seeded(&sp);
    let (code, _, stderr) = run(&sp, &["lock", "on"]);
    assert_eq!(code, Some(1), "{stderr}");
    let head = if cfg!(windows) {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden/lock/lock_on_fallback_sandbox_windows.golden"),
        )
        .unwrap()
    } else {
        SANDBOX_HEAD_UNIX.to_string()
    };
    let stderr = stderr.replace("\r\n", "\n");
    assert!(stderr.contains(&head), "{stderr}");
    assert!(
        stderr.contains("apprafter::cli::usage_refused")
            && stderr.contains("asks for a new lock password"),
        "{stderr}"
    );
    assert_eq!(store_yaml(&sp), None);
}

/// C26: on a store in files `lock off` has nothing to do.
#[test]
fn lock_off_on_a_store_in_files_says_so() {
    let sp = Spawn::new();
    seeded(&sp);
    let (code, stdout, stderr) = run(&sp, &["lock", "off"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("is not locked: its secrets are already in files."),
        "{stdout}"
    );
}

/// §3.11: `lock off` asks a slot's secret; no terminal is `store_locked`, nothing changed.
#[test]
fn lock_off_without_a_terminal_is_store_locked() {
    let sp = Spawn::new();
    locked_store(&sp.store(), &[("prod", Some(TOKEN))]);
    let before = store_yaml(&sp);
    let (code, _, stderr) = run(&sp, &["lock", "off"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("apprafter::secrets::store_locked"),
        "{stderr}"
    );
    assert_eq!(store_yaml(&sp), before);
    assert!(sp.store().join("vault").exists());
}

/// C18: a locked store is not locked again; nothing is asked.
#[test]
fn lock_on_on_a_locked_store_says_so() {
    let sp = Spawn::new();
    locked_store(&sp.store(), &[("prod", Some(TOKEN))]);
    let (code, stdout, stderr) = run(&sp, &["lock", "on", "--lock-password"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("is already locked."), "{stdout}");
}

#[test]
fn a_window_longer_than_a_day_is_a_usage_error() {
    let sp = Spawn::new();
    let (code, _, stderr) = run(&sp, &["lock", "on", "--lock-password", "--timeout", "1441"]);
    assert_eq!(code, Some(2), "{stderr}");
}

/// T5 for the lock leaves: they read or write the shared age key, so the fence checks it on a
/// locked store too (every other command skips it there, Task 3).
#[test]
fn the_lock_leaves_fence_the_age_key_on_a_locked_store() {
    let outside = tempfile::tempdir().unwrap();
    let sp = Spawn::new().with_env("APPRAFTER_AGE_KEY", outside.path().join("age.key"));
    locked_store(&sp.store(), &[("prod", Some(TOKEN))]);
    let (code, _, stderr) = run(&sp, &["lock", "off"]);
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("apprafter::sandbox::escape"), "{stderr}");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

/// L5 (T23, critic S2): a test build creates no password slot outside its sandbox — refused
/// before anything is asked — and says it is one on stderr (T23's exact line); a release
/// build has neither and asks for a terminal. The fence is removed for this case alone
/// (`spawn_guard.rs` `ALLOWED`); every other variable still points into the scratch root.
#[test]
fn a_test_build_locks_no_store_outside_its_sandbox() {
    const NOTE: &str = "INSECURE-FAST-KDF: this test build derives keys with weak parameters\n";
    let sp = Spawn::new().without_env("APPRAFTER_SANDBOX");
    seeded(&sp);
    let (code, _, stderr) = run(&sp, &["lock", "on", "--lock-password"]);
    assert_eq!(code, Some(1), "{stderr}");
    if KdfPolicy::active().is_release() {
        assert!(stderr.contains("apprafter::cli::usage_refused"), "{stderr}");
        assert!(!stderr.contains("INSECURE-FAST-KDF"), "{stderr}");
    } else {
        assert!(
            stderr.contains("apprafter::secrets::insecure_kdf_refused"),
            "{stderr}"
        );
        assert!(stderr.contains(NOTE), "{stderr}");
    }
    assert_eq!(store_yaml(&sp), None);
    assert!(!sp.store().join("vault").exists());
}

#[cfg(unix)]
mod at_a_terminal {
    use super::*;
    use common::terminal::{converse, line, shown_recovery_key, Ended, Step};
    use common::transcript::{check_transcript, normalize, transcript};

    const NEW: &str = "New lock password for ";
    const REPEAT: &str = "Repeat the lock password: ";
    const CONFIRM: &str = "Type the recovery key to confirm you saved it";
    const SECRET: &str = "Lock password or recovery key of ";

    fn type_key(terminal: &str) -> Vec<u8> {
        format!("{}\r", shown_recovery_key(terminal)).into_bytes()
    }

    /// `lock on`'s dialogue with PASSWORD and the key typed back (D.4a's).
    fn lock_steps() -> Vec<Step> {
        vec![
            Step::Expect("Lock the secrets of the target store at "),
            Step::Expect(NEW),
            line(PASSWORD),
            Step::Expect(REPEAT),
            line(PASSWORD),
            Step::Expect(CONFIRM),
            Step::TypeFrom(type_key),
        ]
    }

    /// `lock on --lock-password` with PASSWORD and the key typed back (G-LOCKON-D4A: D.4b B58).
    fn lock_at_terminal(sp: &Spawn) -> Ended {
        converse(sp, &["lock", "on", "--lock-password"], &lock_steps())
    }

    const YAML: &str = "apiVersion: v1\nkind: Config\nclusters: []\n";

    /// `seeded`, and `prod`'s kubeconfig cached under a shared age key made at `key`.
    fn seeded_with_cache(sp: &Spawn, key: &std::path::Path) -> std::path::PathBuf {
        seeded(sp);
        let id = cli_core::secrets::load_or_create_identity(key).unwrap();
        let sealed = cli_core::secrets::encrypt_for_recipient(YAML, &id.to_public()).unwrap();
        let paths = cli_core::TargetStorePaths::for_root(sp.store());
        let state = cli_state::StatePaths::for_active_target(&paths, "prod");
        std::fs::create_dir_all(state.state_dir()).unwrap();
        let body = serde_json::json!({
            "hetzner_cloud": {"server_id": 7, "server_name": "n", "kubeconfig_age": sealed},
        });
        std::fs::write(state.state_file(), body.to_string()).unwrap();
        state.state_file()
    }

    /// T36, O12: the plan, the password twice, the key shown once at the terminal and typed
    /// back whole; the token leaves its file, the key never reaches stdout.
    #[test]
    fn lock_on_asks_twice_shows_the_key_once_and_takes_it_back_whole() {
        let _one = one_at_a_time();
        let sp = Spawn::new();
        seeded(&sp);
        let end = lock_at_terminal(&sp);
        assert!(end.status.success(), "{}", end.terminal);
        assert!(end.restored);
        assert!(
            end.stdout.contains("Locked: the secrets of"),
            "{}",
            end.stdout
        );
        assert!(
            !end.terminal.contains("Use it anyway"),
            "a 28-character password draws no warning (O2)"
        );
        let key = shown_recovery_key(&end.terminal);
        let groups = key.split_whitespace().nth(1).unwrap();
        assert!(!end.stdout.contains(groups), "the key reached stdout");
        assert!(store_yaml(&sp).unwrap().contains("secrets: locked"));
        assert!(
            holding(&sp.store(), TOKEN).is_empty(),
            "the token stayed in clear"
        );
        // G-LOCKON-D4A (D.4b B58, B71, B93): the whole dialogue, byte for byte.
        check_transcript("lock/lock_on_lock_password", &sp, &end);
    }

    /// O2 (overview §5 d): a 14-character password shows C13 and `Use it anyway? [y/N]`;
    /// Enter returns to the new password, `y` goes on, and the store locks with it.
    #[test]
    fn a_short_password_is_warned_enter_starts_again_and_y_locks() {
        const SHORT: &str = "fourteen chars";
        let _one = one_at_a_time();
        let sp = Spawn::new();
        seeded(&sp);
        let end = converse(
            &sp,
            &["lock", "on", "--lock-password"],
            &[
                Step::Expect(NEW),
                line(SHORT),
                Step::Expect(REPEAT),
                line(SHORT),
                Step::Expect("This lock password has 14 characters; 15 or more is recommended."),
                Step::Expect("Use it anyway? [y/N] "),
                line(""),
                Step::Expect(NEW),
                line(SHORT),
                Step::Expect(REPEAT),
                line(SHORT),
                Step::Expect("Use it anyway? [y/N] "),
                line("y"),
                Step::Expect(CONFIRM),
                Step::TypeFrom(type_key),
            ],
        );
        assert!(end.status.success(), "{}", end.terminal);
        assert!(end.restored);
        assert!(store_yaml(&sp).unwrap().contains("secrets: locked"));
        assert!(
            holding(&sp.store(), TOKEN).is_empty(),
            "the token stayed in clear"
        );
    }

    /// O2, T62: empty and recovery-key-shaped passwords are asked again; Ctrl-C at the key's
    /// confirmation (G7) stops the run with nothing written, and the terminal put back.
    #[test]
    fn empty_and_key_shaped_passwords_are_asked_again_and_a_cancel_writes_nothing() {
        let sp = Spawn::new();
        seeded(&sp);
        let end = converse(
            &sp,
            &["lock", "on", "--lock-password"],
            &[
                Step::Expect(NEW),
                line(""),
                Step::Expect("The lock password cannot be empty."),
                Step::Expect(NEW),
                line("qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq"),
                Step::Expect("That has the shape of a recovery key"),
                Step::Expect(NEW),
                line(PASSWORD),
                Step::Expect(REPEAT),
                line(PASSWORD),
                Step::Expect(CONFIRM),
                Step::Type(vec![0x03]),
            ],
        );
        assert_eq!(end.status.code(), Some(1), "{}", end.terminal);
        assert!(
            end.terminal.contains("apprafter::op::cancelled"),
            "{}",
            end.terminal
        );
        assert!(end.restored);
        assert_eq!(store_yaml(&sp), None);
        assert!(!sp.store().join("vault").exists());
    }

    /// O12: a mistyped key is told and asked again; the right one goes on.
    #[test]
    fn a_mistyped_recovery_key_is_asked_again() {
        let _one = one_at_a_time();
        let sp = Spawn::new();
        seeded(&sp);
        let other =
            apprafter_core::vault::RecoveryKey::generate(&mut apprafter_core::vault::OsRandom)
                .unwrap()
                .to_display()
                .expose()
                .replace('\n', " ");
        let end = converse(
            &sp,
            &["lock", "on", "--lock-password"],
            &[
                Step::Expect(NEW),
                line(PASSWORD),
                Step::Expect(REPEAT),
                line(PASSWORD),
                Step::Expect(CONFIRM),
                line(&other),
                Step::Expect("That is not the recovery key shown. Type it again"),
                Step::Expect(CONFIRM),
                Step::TypeFrom(type_key),
            ],
        );
        assert!(end.status.success(), "{}", end.terminal);
        assert!(store_yaml(&sp).unwrap().contains("secrets: locked"));
    }

    /// T37, O13: `lock off` shows the plan, asks one slot's secret and puts the token back;
    /// with a cache to write back, the caches' key is created where `APPRAFTER_AGE_KEY`
    /// points, and the plan says so.
    #[test]
    fn lock_off_puts_the_secrets_back_under_the_key_apprafter_age_key_names() {
        let _one = one_at_a_time();
        let sp = Spawn::new();
        let dest = sp.path("keys").join("age.key");
        let sp = sp.with_env("APPRAFTER_AGE_KEY", &dest);
        let state_file = seeded_with_cache(&sp, &dest);
        assert!(lock_at_terminal(&sp).status.success());
        // The cache opens with the store's own key now; with the shared key gone, `lock off`
        // creates one where APPRAFTER_AGE_KEY points (O13), says so, and seals the cache to it.
        std::fs::remove_file(&dest).unwrap();
        let shown = dest.display().to_string();
        let end = converse(
            &sp,
            &["lock", "off"],
            &[
                Step::Expect("Put the secrets of the target store at "),
                Step::Expect("  create  shared age key "),
                Step::Expect("The secrets will be on disk in clear again"),
                Step::Expect(SECRET),
                line(PASSWORD),
            ],
        );
        assert!(end.status.success(), "{}", end.terminal);
        assert!(end.terminal.contains(&shown), "{}", end.terminal);
        assert!(end.stdout.contains("Unlocked for good:"), "{}", end.stdout);
        assert!(dest.exists());
        let id = cli_core::secrets::load_identity(&dest).unwrap().unwrap();
        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_file).unwrap()).unwrap();
        let armored = state["hetzner_cloud"]["kubeconfig_age"].as_str().unwrap();
        assert_eq!(
            cli_core::secrets::decrypt_with_identity(armored, &id).unwrap(),
            YAML
        );
        assert!(!sp.store().join("vault").exists());
        let creds = std::fs::read_to_string(
            sp.store()
                .join("targets")
                .join("prod")
                .join("credentials.yaml"),
        )
        .unwrap();
        assert!(creds.contains(TOKEN), "{creds}");
    }

    /// T61: three wrong secrets are `unlock_failed`; the store stays locked.
    #[test]
    fn three_wrong_secrets_leave_the_store_locked() {
        let _one = one_at_a_time();
        let sp = Spawn::new();
        seeded(&sp);
        assert!(lock_at_terminal(&sp).status.success());
        let wrong = "That is neither the lock password nor the recovery key of this store.";
        let end = converse(
            &sp,
            &["lock", "off"],
            &[
                Step::Expect(SECRET),
                line("not it, 1"),
                Step::Expect(wrong),
                Step::Expect(SECRET),
                line("not it, 2"),
                Step::Expect(wrong),
                Step::Expect(SECRET),
                line("not it, 3"),
                Step::Expect(wrong),
            ],
        );
        assert_eq!(end.status.code(), Some(1), "{}", end.terminal);
        assert!(
            end.terminal.contains("apprafter::secrets::unlock_failed"),
            "{}",
            end.terminal
        );
        assert!(end.restored);
        assert!(store_yaml(&sp).unwrap().contains("secrets: locked"));
    }

    /// A resume's two questions (rule 4; team decision 2026-10-13).
    const RESUME_PASSWORD: &str = "Lock password of ";
    const RESUME_KEY: &str = "Recovery key of ";

    /// `sp`'s locked store as a `lock on` interrupted with its vault on disk leaves it for a
    /// resume: `store.yaml` back to `locking`, its id and its vault kept (every later step of
    /// the run is idempotent, so a run that got further reads the same).
    fn interrupted(sp: &Spawn) {
        let root = sp.store();
        let held =
            cli_core::StoreLock::exclusive(&cli_core::TargetStorePaths::for_root(root.clone()))
                .unwrap();
        let mut file = apprafter_core::secrets::read_store_file(&root)
            .unwrap()
            .unwrap();
        file.secrets = apprafter_core::secrets::SecretsMode::Locking;
        apprafter_core::secrets::write_store_file(&root, &file, &held).unwrap();
    }

    /// C20, T36, rule 4 (team decision 2026-10-13): an interrupted `lock on` with its vault
    /// finishes only once that vault opened with the lock password AND the recovery key, each
    /// asked by its own question.
    #[test]
    fn an_interrupted_lock_on_finishes_with_both_secrets() {
        let _one = one_at_a_time();
        let sp = Spawn::new();
        seeded(&sp);
        let first = lock_at_terminal(&sp);
        assert!(first.status.success(), "{}", first.terminal);
        let key = shown_recovery_key(&first.terminal);
        interrupted(&sp);
        let end = converse(
            &sp,
            &["lock", "on", "--lock-password"],
            &[
                Step::Expect("did not finish; finishing it."),
                Step::Expect(RESUME_PASSWORD),
                line(PASSWORD),
                Step::Expect(RESUME_KEY),
                line(&key),
            ],
        );
        assert!(end.status.success(), "{}", end.terminal);
        assert!(end.restored);
        assert!(store_yaml(&sp).unwrap().contains("secrets: locked"));
        assert!(
            holding(&sp.store(), TOKEN).is_empty(),
            "the token stayed in clear"
        );
        // G-LOCKON-D4A: the resume's dialogue, byte for byte.
        check_transcript("lock/lock_on_lock_password_resume", &sp, &end);
    }

    /// Rule 4 (team decision 2026-10-13): without the recovery key a resume finishes nothing.
    /// Enter alone at its question tells the way out — `lock off` backs out, a new `lock on`
    /// locks again — and refuses with `lock_incomplete`; `store.yaml` and the vault stay.
    #[test]
    fn a_resume_without_the_recovery_key_names_the_way_out() {
        let _one = one_at_a_time();
        let sp = Spawn::new();
        seeded(&sp);
        assert!(lock_at_terminal(&sp).status.success());
        interrupted(&sp);
        let before = store_yaml(&sp);
        let end = converse(
            &sp,
            &["lock", "on", "--lock-password"],
            &[
                Step::Expect(RESUME_PASSWORD),
                line(PASSWORD),
                Step::Expect(RESUME_KEY),
                line(""),
                // The way out's own words first: `lock_incomplete`'s help repeats the next line.
                Step::Expect("finishes only with both the lock password and the recovery key"),
                Step::Expect("`apprafter lock off` backs out of it with the other"),
            ],
        );
        assert_eq!(end.status.code(), Some(1), "{}", end.terminal);
        assert!(
            end.terminal.contains("apprafter::secrets::lock_incomplete"),
            "{}",
            end.terminal
        );
        assert!(end.restored);
        assert_eq!(store_yaml(&sp), before, "nothing was written");
        assert!(sp.store().join("vault").exists());
    }

    /// D.4a f Task 3, O13: a store with no cache moves none, so neither `lock on` nor `lock
    /// off` creates a shared age key — not where `APPRAFTER_AGE_KEY` points, not at the
    /// default — and neither names one.
    #[test]
    fn lock_on_and_off_without_a_cache_never_touch_the_shared_key() {
        let _one = one_at_a_time();
        let sp = Spawn::new();
        let dest = sp.path("keys").join("age.key");
        let default = sp.home().join(".config").join("apprafter").join("age.key");
        let sp = sp.with_env("APPRAFTER_AGE_KEY", &dest);
        seeded(&sp);
        let shown = dest.display().to_string();
        let on = lock_at_terminal(&sp);
        assert!(on.status.success(), "{}", on.terminal);
        assert!(!on.terminal.contains(&shown), "{}", on.terminal);
        let end = converse(
            &sp,
            &["lock", "off"],
            &[
                Step::Expect("Put the secrets of the target store at "),
                Step::Expect("The secrets will be on disk in clear again"),
                Step::Expect(SECRET),
                line(PASSWORD),
            ],
        );
        assert!(end.status.success(), "{}", end.terminal);
        assert!(end.stdout.contains("Unlocked for good:"), "{}", end.stdout);
        assert!(!end.terminal.contains("shared age key"), "{}", end.terminal);
        assert!(!end.terminal.contains(&shown) && !end.stdout.contains(&shown));
        assert!(!dest.exists() && !default.exists());
    }

    /// D.4b B55, B82, B88 (1) (d4b-e1a): a plain `lock on` where APPRAFTER_SANDBOX fences this
    /// account — every spawned test's — asks this computer nothing, says so first and takes a
    /// lock password: the fence's two lines, then D.4a's dialogue byte for byte
    /// (`lock_on_lock_password`, G-LOCKON-D4A).
    #[test]
    fn a_plain_lock_on_under_the_fence_takes_a_lock_password_and_says_why() {
        let _one = one_at_a_time();
        let sp = Spawn::new();
        seeded(&sp);
        let end = converse(&sp, &["lock", "on"], &lock_steps());
        assert!(end.status.success(), "{}", end.terminal);
        check_transcript("lock/lock_on_fallback_sandbox_unix", &sp, &end);
        let d4a = std::fs::read_to_string(
            common::golden::golden_root().join("lock/lock_on_lock_password.golden"),
        )
        .unwrap();
        assert_eq!(
            transcript(&sp, &end).replacen(SANDBOX_HEAD_UNIX, "", 1),
            d4a,
            "the fence's two lines, then D.4a's dialogue"
        );
    }

    /// D.4b-e1a: the transcript harness masks what varies per run — the root, the recovery
    /// key's groups, a carriage return, a test build's line — and nothing else.
    #[test]
    fn a_transcript_masks_what_varies_and_nothing_else() {
        let sp = Spawn::new();
        let root = sp.root().display().to_string();
        let raw = format!(
            "INSECURE-FAST-KDF: this test build derives keys with weak parameters\r\n\
             Recovery key for {root}/s — shown only now:\r\n\r\n    APPRAFTER-RECOVERY-KEY-1\r\n\
             \x20   QQQSY-QCYQ5\r\n\r\nIt opens this store.\r\n"
        );
        assert_eq!(
            normalize(&sp, &raw),
            "Recovery key for <SANDBOX>/s — shown only now:\n\n    APPRAFTER-RECOVERY-KEY-1\n    \
             <RECOVERY-KEY>\n\nIt opens this store.\n"
        );
        assert_eq!(normalize(&sp, "Lock the secrets\n"), "Lock the secrets\n");
    }
}
