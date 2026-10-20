// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The fake `kubectl` of the read goldens (D.5 overview §6.1, d5-a2), and the target stores the
//! cases read a cluster through.
//!
//! [`FakeKubectl::install`] writes a `/bin/sh` script to the sandbox's `bin`, the child's whole
//! `PATH` (`common::spawn`), so it is the only `kubectl` the CLI can start. It uses shell
//! builtins alone — a `read` loop prints each answer from its file (GOTCHA-104) — and reaches
//! nothing: no network, no other program. It
//!
//! - logs each call's whole argv to `argv.log` (overview §6.1): one `+<argument>` line per
//!   argument, then `.` ([`argv`]);
//! - drops the client's own flags — `--kubeconfig <file>`, `--cache-dir <dir>` (or their `=`
//!   forms) and `--request-timeout=<n>s` — so the argv D.5's `KubectlKube` builds (B1, B73)
//!   meets the answers today's bare `kubectl get …` meets;
//! - logs each call's remaining arguments as one key, each after a `|` (`|get|nodes|-o|json`;
//!   the roll-up's empty selector, `-l ""`, shows as `||`), to `calls.log`;
//! - refuses, exit 3, when `KUBECONFIG` names no file: the CLI sets it to the kubeconfig it
//!   materialised for every read, so a call without one is a path that skipped the store;
//! - answers the arm whose key matches (exactly, or as a prefix for an argument that carries a
//!   time — the re-check's patch body), else the `otherwise` failure, else exits 2.
//!
//! Every call it does not answer as the table says — no arm and no `otherwise`, or the
//! KUBECONFIG refusal — is a line of `unanswered.log`, which every case asserts is absent: a
//! leaf that swallows a failed read (`node status` prints `unknown`) still fails its case.
//!
//! The kubeconfig every store here caches names `https://127.0.0.1:1`, where nothing listens:
//! a real kubectl that somehow ran would still reach no cluster.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::golden::{Sandbox, TOKEN_A};

/// kubectl's words for an apiserver port that refuses the connection (GOTCHA-122).
pub const REFUSED: &str =
    "The connection to the server 127.0.0.1:1 was refused - did you specify the right host or port?";
/// kubectl's words when the cached credentials no longer open the cluster (a 401, B70).
pub const UNAUTHORIZED: &str = "error: You must be logged in to the server (Unauthorized)";
/// What every cached kubeconfig here holds: a cluster on a port nothing listens on.
pub const KUBECONFIG_YAML: &str = "apiVersion: v1\nkind: Config\nclusters:\n- name: golden\n  cluster:\n    server: https://127.0.0.1:1\ncontexts:\n- name: golden\n  context:\n    cluster: golden\n    user: golden\ncurrent-context: golden\nusers:\n- name: golden\n  user:\n    token: golden\n";

/// The `read`-loop printer each fake defines first: builtins only (GOTCHA-104).
pub(crate) const EMIT: &str =
    r#"emit() { while IFS= read -r l || [ -n "$l" ]; do printf '%s\n' "$l"; done < "$1"; }"#;
/// What the fake does with a call no arm and no `otherwise` answers. Messages go through
/// `printf`, never `echo`, which dash lets interpret a backslash in the key.
const UNANSWERED: &str = r#"printf '%s\n' "$key" >> "$d/unanswered.log"; printf 'fake kubectl: no answer for %s\n' "$key" >&2; exit 2"#;
/// Each call's whole argv, before anything is dropped (overview §6.1; [`read_argv`]).
pub(crate) const LOG_ARGV: &str =
    r#"{ for a in "$@"; do printf '+%s\n' "$a"; done; printf '.\n'; } >> "$d/argv.log""#;

/// One answer: stdout and exit 0, or stderr (one line, none when empty) and an exit code.
#[derive(Debug, Clone)]
pub(crate) enum Reply {
    Out(String),
    Fail { exit: i32, stderr: String },
}

#[derive(Debug, Clone)]
struct Arm {
    key: String,
    prefix: bool,
    reply: Reply,
}

/// The case table of a fake `kubectl` (module docs).
#[derive(Debug, Clone, Default)]
pub struct FakeKubectl {
    arms: Vec<Arm>,
    otherwise: Option<Reply>,
}

/// The key a call of `argv` logs and matches: each argument after a `|`.
pub fn key(argv: &[&str]) -> String {
    argv.iter().map(|a| format!("|{a}")).collect()
}

impl FakeKubectl {
    pub fn new() -> Self {
        Self::default()
    }

    /// `argv` prints `body` as pretty JSON, exit 0.
    pub fn json(self, argv: &[&str], body: &Value) -> Self {
        let text = serde_json::to_string_pretty(body).expect("a JSON answer") + "\n";
        self.arm(argv, false, Reply::Out(text))
    }

    /// `argv` prints `body` as it is, exit 0: a jsonpath or `--raw` answer.
    pub fn text(self, argv: &[&str], body: &str) -> Self {
        self.arm(argv, false, Reply::Out(body.to_string()))
    }

    /// `argv` fails: `stderr` (one line, or none when empty) and `exit`.
    pub fn fail(self, argv: &[&str], exit: i32, stderr: &str) -> Self {
        let reply = Reply::Fail {
            exit,
            stderr: stderr.to_string(),
        };
        self.arm(argv, false, reply)
    }

    /// Any call whose arguments start with `argv` succeeds and prints nothing.
    pub fn ok_prefix(self, argv: &[&str]) -> Self {
        self.arm(argv, true, Reply::Out(String::new()))
    }

    /// Every call no arm answers fails so: a cluster that refuses every read.
    pub fn otherwise_fail(mut self, exit: i32, stderr: &str) -> Self {
        self.otherwise = Some(Reply::Fail {
            exit,
            stderr: stderr.to_string(),
        });
        self
    }

    /// An arm for a key the table already has replaces it, so a case can start from a whole
    /// cluster and change one answer.
    fn arm(mut self, argv: &[&str], prefix: bool, reply: Reply) -> Self {
        let key = key(argv);
        assert!(
            !key.contains('\''),
            "a fake kubectl key holds no single quote: {key}"
        );
        self.arms.retain(|a| !(a.key == key && a.prefix == prefix));
        self.arms.push(Arm { key, prefix, reply });
        self
    }

    /// Writes the fake to `<sandbox>/bin/kubectl`, its answers and logs under
    /// `<sandbox>/fakes/kubectl`.
    pub fn install(&self, sb: &Sandbox) {
        let dir = sb.path("fakes/kubectl");
        fs::create_dir_all(&dir).expect("the fake kubectl's directory");
        let mut lines = vec![
            format!("d='{}'", quoted(&dir)),
            EMIT.to_string(),
            LOG_ARGV.to_string(),
        ];
        lines.extend(
            [
                r#"key="#,
                r#"skip="#,
                r#"for a in "$@"; do"#,
                r#"  if [ -n "$skip" ]; then skip=; continue; fi"#,
                r#"  case "$a" in"#,
                r#"    --kubeconfig|--cache-dir) skip=1; continue ;;"#,
                r#"    --kubeconfig=*|--cache-dir=*|--request-timeout=*) continue ;;"#,
                r#"  esac"#,
                r#"  key="$key|$a""#,
                r#"done"#,
                r#"printf '%s\n' "$key" >> "$d/calls.log""#,
                r#"if [ ! -f "$KUBECONFIG" ]; then"#,
                r#"  printf 'KUBECONFIG names no file: %s\n' "$key" >> "$d/unanswered.log""#,
                r#"  printf '%s\n' 'fake kubectl: KUBECONFIG names no file' >&2"#,
                r#"  exit 3"#,
                r#"fi"#,
                r#"case "$key" in"#,
            ]
            .map(str::to_string),
        );
        for (i, arm) in self.arms.iter().enumerate() {
            let glob = if arm.prefix { "*" } else { "" };
            let reply = reply_code(&dir, &i.to_string(), &arm.reply);
            lines.push(format!("'{}'{glob}) {reply} ;;", arm.key));
        }
        let fallback = match &self.otherwise {
            Some(reply) => reply_code(&dir, "otherwise", reply),
            None => UNANSWERED.to_string(),
        };
        lines.push(format!("*) {fallback} ;;"));
        lines.push("esac".to_string());
        super::stand_in::script(&sb.path("bin/kubectl"), &lines.join("\n"));
    }
}

/// Every call the fake got, as its key, in order.
pub fn calls(sb: &Sandbox) -> Vec<String> {
    read_lines(&sb.path("fakes/kubectl/calls.log"))
}

/// Each call's whole argv, the client's flags included, in order.
pub fn argv(sb: &Sandbox) -> Vec<Vec<String>> {
    read_argv(&sb.path("fakes/kubectl/argv.log"))
}

/// The calls no arm and no `otherwise` answered, and the ones refused for want of a
/// kubeconfig (`KUBECONFIG names no file: <key>`).
pub fn unanswered(sb: &Sandbox) -> Vec<String> {
    read_lines(&sb.path("fakes/kubectl/unanswered.log"))
}

/// Every call the fake got had an answer.
pub fn assert_all_answered(sb: &Sandbox) {
    let missing = unanswered(sb);
    assert!(
        missing.is_empty(),
        "the fake kubectl had no answer for {missing:?}"
    );
}

/// A log's lines; none when the fake never wrote it.
pub(crate) fn read_lines(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

/// An `argv.log` ([`LOG_ARGV`]): `+<argument>` lines, each call ended by `.`. No argument
/// can end a call early (`--` included, which an ssh argv may carry).
pub(crate) fn read_argv(path: &Path) -> Vec<Vec<String>> {
    let mut calls = Vec::new();
    let mut call = Vec::new();
    for line in read_lines(path) {
        match line.strip_prefix('+') {
            Some(argument) => call.push(argument.to_string()),
            None => {
                assert_eq!(line, ".", "{}: a line that is no argument", path.display());
                calls.push(std::mem::take(&mut call));
            }
        }
    }
    calls
}

/// The shell that gives `reply`, its text written to `<dir>/<name>.out` or `.err`.
pub(crate) fn reply_code(dir: &Path, name: &str, reply: &Reply) -> String {
    match reply {
        Reply::Out(text) if text.is_empty() => "exit 0".to_string(),
        Reply::Out(text) => {
            let file = dir.join(format!("{name}.out"));
            fs::write(&file, text).expect("an answer file");
            format!("emit '{}'; exit 0", quoted(&file))
        }
        Reply::Fail { exit, stderr } if stderr.is_empty() => format!("exit {exit}"),
        Reply::Fail { exit, stderr } => {
            let file = dir.join(format!("{name}.err"));
            fs::write(&file, format!("{stderr}\n")).expect("an answer file");
            format!("emit '{}' >&2; exit {exit}", quoted(&file))
        }
    }
}

/// `path` for a single-quoted shell word: it must hold no single quote.
pub(crate) fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(
        !text.contains('\''),
        "a fake's path holds no single quote: {text}"
    );
    text
}

// ---------------------------------------------------------------------
// The stores the cases read a cluster through
// ---------------------------------------------------------------------

/// What every provisioned target here records: its cluster's name and provider, and server
/// 42 (`prod-node`, which the node cases' provider lists at 203.0.113.10), with the
/// kubeconfig cache `armored` when given, and the locked store's id `vault` when the cache
/// was sealed to that store's key, as `lock on` records it (`kubeconfig_sealed_to`,
/// GOTCHA-308; critic round, finding 14).
fn provisioned_state(armored: Option<&str>, vault: Option<&str>) -> String {
    let mut server = serde_json::json!({
        "server_id": 42,
        "server_name": "prod-node",
        "server_type": "cx22"
    });
    if let Some(armored) = armored {
        server["kubeconfig_age"] = serde_json::json!(armored);
    }
    if let Some(store_id) = vault {
        server["kubeconfig_sealed_to"] =
            serde_json::json!({ "kind": "store_vault", "store_id": store_id });
    }
    serde_json::json!({
        "cluster_name": "apprafter-prod",
        "provider": "hetzner-cloud",
        "hetzner_cloud": server
    })
    .to_string()
}

/// Where the CLI finds the store's age key (`HOME` is `<sandbox>/home`).
fn age_key_path(sb: &Sandbox) -> PathBuf {
    sb.path("home/.config/apprafter/age.key")
}

/// `name`, added as `Sandbox::add_target` adds it (the active target when it is the first),
/// provisioned, its kubeconfig cached under the store's age key, made here.
pub fn cluster_target(sb: &Sandbox, name: &str) {
    sb.add_target(name);
    let key = cli_core::secrets::load_or_create_identity(&age_key_path(sb)).expect("the age key");
    let armored = cli_core::secrets::encrypt_for_recipient(KUBECONFIG_YAML, &key.to_public())
        .expect("the kubeconfig, sealed");
    sb.seed_state(name, &provisioned_state(Some(&armored), None));
}

/// `name`, provisioned, no kubeconfig cached.
pub fn provisioned_target(sb: &Sandbox, name: &str) {
    sb.add_target(name);
    sb.seed_state(name, &provisioned_state(None, None));
}

/// `name`, provisioned, its kubeconfig cached under a key that is on no disk.
pub fn cluster_target_key_lost(sb: &Sandbox, name: &str) {
    sb.add_target(name);
    let lost = age::x25519::Identity::generate();
    let armored = cli_core::secrets::encrypt_for_recipient(KUBECONFIG_YAML, &lost.to_public())
        .expect("the kubeconfig, sealed");
    sb.seed_state(name, &provisioned_state(Some(&armored), None));
}

/// `name`, provisioned, its kubeconfig cached under another key than the store's, which is
/// on disk.
pub fn cluster_target_other_key(sb: &Sandbox, name: &str) {
    sb.add_target(name);
    cli_core::secrets::load_or_create_identity(&age_key_path(sb)).expect("the age key");
    let other = age::x25519::Identity::generate();
    let armored = cli_core::secrets::encrypt_for_recipient(KUBECONFIG_YAML, &other.to_public())
        .expect("the kubeconfig, sealed");
    sb.seed_state(name, &provisioned_state(Some(&armored), None));
}

/// A locked store (D.4a) holding `name`, its token in the vault, the CLI default, provisioned,
/// its kubeconfig sealed to the store's own key as a lock leaves it.
pub fn locked_cluster_target(sb: &Sandbox, name: &str) {
    let made = apprafter_core::secrets::testing::locked_store(
        &sb.path("apprafter-config"),
        &[(name, Some(TOKEN_A))],
    );
    sb.seed_pointer(name);
    let armored = cli_core::secrets::encrypt_for_recipient(KUBECONFIG_YAML, made.recipient())
        .expect("the kubeconfig, sealed");
    let vault = Some(made.store_id.as_str());
    sb.seed_state(name, &provisioned_state(Some(&armored), vault));
}
