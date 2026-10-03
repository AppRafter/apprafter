// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The status commands that report the provisioner's latest abandoned
//! reconcile (WI-400), end to end through the shipped binary.
//!
//! Nothing here reaches a cluster. The child's `PATH` holds one program, a
//! `kubectl` stand-in that logs its arguments and answers each exact
//! argument list from a fixture (anything else fails as NotFound). The
//! cached kubeconfig is a placeholder that no real kubectl reads, the
//! ambient `KUBECONFIG` points nowhere, and the startup checks are off.
//!
//! The unit tests pin the text. These pin the wiring that no unit test can
//! see: that each command reads its object with its `managedFields`, then
//! lists the right Events, goes quiet once the reporting controller has
//! written the status since, and stays quiet when the Event read fails.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use chrono::{Duration, SecondsFormat, Utc};
use serde_json::{json, Value};
use tempfile::TempDir;

/// What the stand-in does for one exact argument list.
enum Reply {
    /// Print this JSON and exit 0.
    Json(Value),
    /// Print this to stderr and exit 1.
    Fail(&'static str),
}

const VOLUME_GET: &str = "get sharedvolume.apprafter.io --show-managed-fields data -n apps -o json";
const VOLUME_EVENTS: &str = "get events.events.k8s.io -n apps --field-selector \
     reason=ReconcileTimedOut,regarding.kind=SharedVolume,regarding.name=data -o json";
const DATABASE_GET: &str =
    "get shareddatabase.apprafter.io --show-managed-fields orders -n apps -o json";
const CLAIMS: &str = "get resourceclaim.apprafter.io -n apps -o json";
const DATABASE_EVENTS: &str = "get events.events.k8s.io -n apps --field-selector \
     reason=ReconcileTimedOut,regarding.kind=SharedDatabase,regarding.name=orders -o json";

struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    /// A config dir with an active target whose state caches a placeholder
    /// kubeconfig, and a `bin/kubectl` that answers `replies`.
    fn new(replies: &[(&str, Reply)]) -> Self {
        let sandbox = Sandbox {
            dir: TempDir::new().unwrap(),
        };
        sandbox.seed_state();
        sandbox.write_kubectl(replies);
        sandbox
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The local-only commands every other integration test uses to make a
    /// target, then the cached kubeconfig `ensure_kubeconfig_tempfile` reads.
    fn seed_state(&self) {
        let local = |args: &[&str]| {
            Command::cargo_bin("apprafter")
                .unwrap()
                .env_remove("HCLOUD_TOKEN")
                .env("HOME", self.path("home"))
                .env("XDG_CONFIG_HOME", self.path("config"))
                .env("APPRAFTER_CONFIG_DIR", self.path("apprafter-config"))
                .env("APPRAFTER_AGE_KEY", self.path("age.key"))
                .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
                .env("KUBECONFIG", "/nonexistent")
                .current_dir(self.dir.path())
                .args(args)
                .assert()
                .success();
        };
        let token = "a".repeat(64);
        local(&[
            "target",
            "add",
            "default",
            "--provider",
            "hetzner-cloud",
            "--token",
            &token,
            "--region",
            "nbg1",
            "--tier",
            "solo",
            "--no-ping",
            "--no-interactive",
        ]);
        local(&[
            "init",
            "--provider",
            "hetzner-cloud",
            "--tier",
            "solo",
            "--region",
            "nbg1",
        ]);
        let state_path = self
            .path("apprafter-config")
            .join("state/default/.apprafter/state.json");
        let mut state: Value =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        state["hetzner_cloud"] = json!({
            "server_id": 1,
            "server_name": "platform-1",
            "ssh_key_ids": [],
            "network_id": null,
            "firewall_id": null,
            "floating_ip_ids": [],
            "kubeconfig_yaml": "apiVersion: v1\nkind: Config\n# read by no real kubectl\n"
        });
        fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    }

    /// A POSIX-shell `kubectl` built from shell builtins only, since the
    /// child's `PATH` holds nothing else.
    fn write_kubectl(&self, replies: &[(&str, Reply)]) {
        let bin = self.path("bin");
        fs::create_dir_all(&bin).unwrap();
        let mut arms = String::new();
        for (i, (args, reply)) in replies.iter().enumerate() {
            match reply {
                Reply::Json(body) => {
                    let file = self.path(&format!("reply-{i}.json"));
                    fs::write(&file, serde_json::to_vec(body).unwrap()).unwrap();
                    arms.push_str(&format!(
                        "  '{args}')\n    while IFS= read -r l || [ -n \"$l\" ]; do \
                         printf '%s\\n' \"$l\"; done < '{}'\n    exit 0 ;;\n",
                        file.display()
                    ));
                }
                Reply::Fail(stderr) => arms.push_str(&format!(
                    "  '{args}')\n    echo '{stderr}' >&2\n    exit 1 ;;\n"
                )),
            }
        }
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$*\" in\n{arms}  *)\n    \
             echo 'Error from server (NotFound): the server could not find the requested \
             resource' >&2\n    exit 1 ;;\nesac\n",
            log = self.path("kubectl.log").display()
        );
        let kubectl = bin.join("kubectl");
        fs::write(&kubectl, script).unwrap();
        fs::set_permissions(&kubectl, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Run `apprafter <args>` with nothing on `PATH` but the stand-in.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::cargo_bin("apprafter")
            .unwrap()
            .env_clear()
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("APPRAFTER_CONFIG_DIR", self.path("apprafter-config"))
            .env("APPRAFTER_AGE_KEY", self.path("age.key"))
            .env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
            .env("KUBECONFIG", "/nonexistent")
            .current_dir(self.dir.path())
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn kubectl_calls(&self) -> Vec<String> {
        fs::read_to_string(self.path("kubectl.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn rfc3339(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// The object `kind/name` as `kubectl get -o json` returns it.
fn object(kind: &str, name: &str, status: Value) -> Value {
    json!({
        "apiVersion": "apprafter.io/v1alpha1", "kind": kind,
        "metadata": { "name": name, "namespace": "apps", "uid": format!("uid-{name}") },
        "spec": { "size": "1Gi", "type": "pg" },
        "status": status
    })
}

/// The provisioner's Event about that object, for a pass abandoned `ago`.
fn abandoned(kind: &str, name: &str, ago: Duration, deadline: u64) -> Value {
    json!({ "items": [{
        "apiVersion": "events.k8s.io/v1", "kind": "Event",
        "metadata": { "name": format!("{name}.17a3b0c2d4e5f607"), "namespace": "apps" },
        "eventTime": rfc3339(Utc::now() - ago),
        "type": "Warning", "reason": "ReconcileTimedOut", "action": "Reconcile",
        "note": format!(
            "{kind} reconcile did not finish within {deadline}s: the controller abandoned \
             this pass and will retry it; nothing was written to this object's status"
        ),
        "regarding": { "apiVersion": "apprafter.io/v1alpha1", "kind": kind,
                       "name": name, "namespace": "apps", "uid": format!("uid-{name}") },
        "reportingController": "apprafter-resourceclaim-provisioner"
    }]})
}

fn volume() -> Value {
    object(
        "SharedVolume",
        "data",
        json!({ "ready": true, "refCount": 2, "pvcRef": "sv-data" }),
    )
}

#[test]
fn volume_status_names_an_abandoned_pass() {
    let sandbox = Sandbox::new(&[
        (VOLUME_GET, Reply::Json(volume())),
        (
            VOLUME_EVENTS,
            Reply::Json(abandoned("SharedVolume", "data", Duration::minutes(3), 60)),
        ),
    ]);
    let (ok, stdout, stderr) = sandbox.run(&["volume", "status", "data", "-n", "apps"]);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains(
            "  Reconcile:   last timed out 3 minutes ago (did not finish within 60s); the \
             operator retries on its own"
        ),
        "{stdout}"
    );
    let calls = sandbox.kubectl_calls();
    assert_eq!(calls, [VOLUME_GET, VOLUME_EVENTS], "{calls:#?}");
}

#[test]
fn volume_status_against_an_operator_without_the_event_prints_nothing_extra() {
    let sandbox = Sandbox::new(&[
        (VOLUME_GET, Reply::Json(volume())),
        (VOLUME_EVENTS, Reply::Json(json!({ "items": [] }))),
    ]);
    let (ok, stdout, stderr) = sandbox.run(&["volume", "status", "data", "-n", "apps"]);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(!stdout.contains("Reconcile"), "{stdout}");
    assert!(stdout.contains("  Ref count:   2"), "{stdout}");
}

#[test]
fn volume_status_is_quiet_once_the_provisioner_has_written_the_status_since() {
    // A status write under the provisioner's own field manager after the
    // Event: a later pass got through. kubectl returns `managedFields` only
    // under `--show-managed-fields`, which `VOLUME_GET` asks for; a plain
    // read would see none, and keep the line until the Event expired.
    let mut recovered = volume();
    recovered["metadata"]["managedFields"] = json!([{
        "manager": "resourceclaim-provisioner", "operation": "Apply",
        "apiVersion": "apprafter.io/v1alpha1", "subresource": "status",
        "time": (Utc::now() - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
        "fieldsType": "FieldsV1", "fieldsV1": { "f:status": {} }
    }]);
    let sandbox = Sandbox::new(&[
        (VOLUME_GET, Reply::Json(recovered)),
        (
            VOLUME_EVENTS,
            Reply::Json(abandoned("SharedVolume", "data", Duration::minutes(3), 60)),
        ),
    ]);
    let (ok, stdout, stderr) = sandbox.run(&["volume", "status", "data", "-n", "apps"]);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(!stdout.contains("Reconcile"), "{stdout}");
    assert!(stdout.contains("  Ref count:   2"), "{stdout}");
    let calls = sandbox.kubectl_calls();
    assert_eq!(calls, [VOLUME_GET, VOLUME_EVENTS], "{calls:#?}");
}

#[test]
fn a_reader_who_may_not_list_events_gets_the_volume_and_no_warning() {
    let sandbox = Sandbox::new(&[
        (VOLUME_GET, Reply::Json(volume())),
        (
            VOLUME_EVENTS,
            Reply::Fail(
                "Error from server (Forbidden): events.events.k8s.io is forbidden: User \
                 \"reader\" cannot list resource \"events\"",
            ),
        ),
    ]);
    let (ok, stdout, stderr) = sandbox.run(&["volume", "status", "data", "-n", "apps"]);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(!stdout.contains("Reconcile"), "{stdout}");
    assert!(stdout.contains("  Ref count:   2"), "{stdout}");
    assert!(stderr.trim().is_empty(), "{stderr}");
}

#[test]
fn db_status_names_an_abandoned_pass() {
    let database = object(
        "SharedDatabase",
        "orders",
        json!({ "ready": true, "refCount": 0, "database": "shd_apps_orders" }),
    );
    let sandbox = Sandbox::new(&[
        (DATABASE_GET, Reply::Json(database)),
        (CLAIMS, Reply::Json(json!({ "items": [] }))),
        (
            DATABASE_EVENTS,
            Reply::Json(abandoned(
                "SharedDatabase",
                "orders",
                Duration::minutes(2),
                120,
            )),
        ),
    ]);
    let (ok, stdout, stderr) = sandbox.run(&["db", "status", "orders", "-n", "apps"]);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains(
            "  Reconcile:    last timed out 2 minutes ago (did not finish within 120s); the \
             operator retries on its own"
        ),
        "{stdout}"
    );
    let calls = sandbox.kubectl_calls();
    assert_eq!(calls, [DATABASE_GET, CLAIMS, DATABASE_EVENTS], "{calls:#?}");
}

#[test]
fn db_status_is_quiet_once_the_provisioner_has_written_the_status_since() {
    // A status write under the provisioner's own field manager after the
    // Event: a later pass got through. kubectl returns `managedFields` only
    // under `--show-managed-fields`, which `DATABASE_GET` asks for; a plain
    // read would see none, and keep the line until the Event expired.
    let mut recovered = object(
        "SharedDatabase",
        "orders",
        json!({ "ready": true, "refCount": 0, "database": "shd_apps_orders" }),
    );
    recovered["metadata"]["managedFields"] = json!([{
        "manager": "resourceclaim-provisioner", "operation": "Apply",
        "apiVersion": "apprafter.io/v1alpha1", "subresource": "status",
        "time": (Utc::now() - Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
        "fieldsType": "FieldsV1", "fieldsV1": { "f:status": {} }
    }]);
    let sandbox = Sandbox::new(&[
        (DATABASE_GET, Reply::Json(recovered)),
        (CLAIMS, Reply::Json(json!({ "items": [] }))),
        (
            DATABASE_EVENTS,
            Reply::Json(abandoned(
                "SharedDatabase",
                "orders",
                Duration::minutes(2),
                120,
            )),
        ),
    ]);
    let (ok, stdout, stderr) = sandbox.run(&["db", "status", "orders", "-n", "apps"]);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(!stdout.contains("Reconcile"), "{stdout}");
    assert!(stdout.contains("  Bound apps:   0"), "{stdout}");
    let calls = sandbox.kubectl_calls();
    assert_eq!(calls, [DATABASE_GET, CLAIMS, DATABASE_EVENTS], "{calls:#?}");
}
