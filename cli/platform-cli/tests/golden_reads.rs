// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Golden snapshots of the six read leaves D.5 moves onto `apprafter-core` — `status`, `top`,
//! `platform status` (with and without `--cached`), `platform autoscale show`, `platform env
//! show` and `node status` (D.5 overview §6.1, d5-a2) — recorded on the binary before any of
//! them moved. Every later change to one of these files is deliberate and its own commit
//! (B8, B54, B75).
//!
//! The cluster is the fake `kubectl` of `common/fake_kube.rs`, the node the fake `ssh` of
//! `common/fake_ssh.rs`, the provider a `mockito` server: nothing reaches a real cluster, host
//! or API. What they answer is built by `apprafter_core::fixtures` at the real clock, each time
//! in the middle of its relative band (B55, B76), right before the command reads it. Each case
//! asserts the fakes had an answer for every call. The harness and its rules are in
//! `common/golden.rs`; record with `APPRAFTER_GOLDEN_UPDATE=1 cargo test -p apprafter --test
//! golden_reads`.
#![cfg(unix)]

mod common;
use common::golden::*;

use apprafter_core::fixtures as fx;

// ---------------------------------------------------------------------
// The fakes' own guarantees
// ---------------------------------------------------------------------

mod fakes {
    use super::*;
    use crate::common::fake_kube::{self, key, FakeKubectl, REFUSED, UNAUTHORIZED};
    use crate::common::fake_ssh::{self, FakeSsh};
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    /// Runs the installed fake `tool` with `args` as the CLI does — `PATH` the sandbox's `bin`
    /// alone, `KUBECONFIG` the file given — and returns (exit, stdout, stderr).
    fn run(
        sb: &Sandbox,
        tool: &str,
        args: &[&str],
        kubeconfig: &Path,
    ) -> (Option<i32>, String, String) {
        let out = std::process::Command::new(sb.path(&format!("bin/{tool}")))
            .args(args)
            .env("PATH", sb.path("bin"))
            .env("KUBECONFIG", kubeconfig)
            .output()
            .expect("run the fake");
        (
            out.status.code(),
            String::from_utf8(out.stdout).expect("UTF-8"),
            String::from_utf8(out.stderr).expect("UTF-8"),
        )
    }

    /// A kubeconfig file in the sandbox, as the CLI materialises one for a read.
    fn kubeconfig(sb: &Sandbox) -> PathBuf {
        let path = sb.path("materialised-kubeconfig");
        std::fs::write(&path, fake_kube::KUBECONFIG_YAML).expect("a kubeconfig");
        path
    }

    /// B1 / B73: the argv D.5's `KubectlKube` builds meets the answers today's bare `kubectl
    /// get …` meets.
    #[test]
    fn kubectl_answers_its_table_whatever_client_flags_come_first() {
        let sb = Sandbox::new();
        let nodes = fx::top::solo_nodes();
        FakeKubectl::new()
            .json(&["get", "nodes", "-o", "json"], &nodes)
            .install(&sb);
        let kc = kubeconfig(&sb);
        for args in [
            &["get", "nodes", "-o", "json"][..],
            &[
                "--kubeconfig",
                "/run/kc",
                "--request-timeout=10s",
                "get",
                "nodes",
                "-o",
                "json",
            ],
            &[
                "--cache-dir",
                "/run/kube-cache",
                "--kubeconfig=/run/kc",
                "get",
                "nodes",
                "-o",
                "json",
            ],
        ] {
            let (code, out, err) = run(&sb, "kubectl", args, &kc);
            assert_eq!((code, err.as_str()), (Some(0), ""), "{args:?}");
            assert_eq!(
                serde_json::from_str::<Value>(&out).expect("JSON"),
                nodes,
                "{args:?}"
            );
        }
        assert_eq!(
            fake_kube::calls(&sb),
            vec![key(&["get", "nodes", "-o", "json"]); 3]
        );
        // §6.1 (critic round, finding 4): the whole argv is logged, the dropped flags too.
        assert_eq!(
            fake_kube::argv(&sb)[1],
            [
                "--kubeconfig",
                "/run/kc",
                "--request-timeout=10s",
                "get",
                "nodes",
                "-o",
                "json"
            ]
        );
        fake_kube::assert_all_answered(&sb);
    }

    /// F4: the roll-up's empty selector is an argument of its own; a prefix arm answers an
    /// argument that carries a time (the re-check's patch body).
    #[test]
    fn kubectl_matches_an_empty_argument_and_a_prefix() {
        let sb = Sandbox::new();
        let listing = [
            "get",
            "application.apprafter.io",
            "-l",
            "",
            "-A",
            "-o",
            "json",
        ];
        let patch = [
            "patch",
            "platformstack",
            "default",
            "-n",
            "apprafter-system",
            "--type=merge",
            "-p",
        ];
        FakeKubectl::new()
            .json(&listing, &fx::list(vec![]))
            .ok_prefix(&patch)
            .install(&sb);
        let kc = kubeconfig(&sb);
        let (code, out, _) = run(&sb, "kubectl", &listing, &kc);
        assert_eq!(code, Some(0));
        assert_eq!(
            serde_json::from_str::<Value>(&out).expect("JSON"),
            fx::list(vec![])
        );
        let body = r#"{"metadata":{"annotations":{"apprafter.io/recheck-requested":"2026-10-16T12:00:00+00:00"}}}"#;
        let patched: Vec<&str> = patch.iter().copied().chain([body]).collect();
        assert_eq!(
            run(&sb, "kubectl", &patched, &kc),
            (Some(0), String::new(), String::new())
        );
        let other = [
            "get",
            "application.apprafter.io",
            "-l",
            "x=y",
            "-A",
            "-o",
            "json",
        ];
        assert_eq!(
            run(&sb, "kubectl", &other, &kc).0,
            Some(2),
            "a selector the table does not name"
        );
        assert_eq!(
            fake_kube::calls(&sb)[0],
            "|get|application.apprafter.io|-l||-A|-o|json"
        );
        assert_eq!(fake_kube::unanswered(&sb), [key(&other)]);
    }

    #[test]
    fn kubectl_fails_as_told_and_records_what_it_could_not_answer() {
        let sb = Sandbox::new();
        let stack = [
            "get",
            "platformstack",
            "default",
            "-n",
            "apprafter-system",
            "-o",
            "json",
        ];
        FakeKubectl::new().fail(&stack, 1, REFUSED).install(&sb);
        let kc = kubeconfig(&sb);
        assert_eq!(
            run(&sb, "kubectl", &stack, &kc),
            (Some(1), String::new(), format!("{REFUSED}\n"))
        );
        let nodes = ["get", "nodes", "-o", "json"];
        assert_eq!(
            run(&sb, "kubectl", &nodes, &kc),
            (
                Some(2),
                String::new(),
                "fake kubectl: no answer for |get|nodes|-o|json\n".to_string()
            )
        );
        assert_eq!(fake_kube::unanswered(&sb), [key(&nodes)]);
    }

    #[test]
    fn kubectl_otherwise_fails_every_other_call() {
        let sb = Sandbox::new();
        FakeKubectl::new()
            .otherwise_fail(1, UNAUTHORIZED)
            .install(&sb);
        let kc = kubeconfig(&sb);
        for args in [
            &["get", "nodes", "-o", "json"][..],
            &["get", "--raw", "/version"],
        ] {
            assert_eq!(
                run(&sb, "kubectl", args, &kc),
                (Some(1), String::new(), format!("{UNAUTHORIZED}\n")),
                "{args:?}"
            );
        }
        fake_kube::assert_all_answered(&sb);
    }

    /// A case starts from a whole cluster and changes one answer.
    #[test]
    fn kubectl_a_later_answer_replaces_an_earlier_one() {
        let sb = Sandbox::new();
        let raw = ["get", "--raw", "/version"];
        FakeKubectl::new()
            .text(&raw, "first\n")
            .fail(&raw, 1, "second")
            .install(&sb);
        let kc = kubeconfig(&sb);
        assert_eq!(
            run(&sb, "kubectl", &raw, &kc),
            (Some(1), String::new(), "second\n".to_string())
        );
    }

    /// A read the CLI makes without the kubeconfig it materialised skipped the store; the
    /// refusal is recorded, so a case whose CLI swallows the failure (`node status`'s
    /// `[api]` reads) still fails `assert_all_answered` (critic round, finding 2).
    #[test]
    fn kubectl_refuses_without_a_kubeconfig_file() {
        let sb = Sandbox::new();
        let raw = ["get", "--raw", "/version"];
        FakeKubectl::new().text(&raw, "{}\n").install(&sb);
        assert_eq!(
            run(&sb, "kubectl", &raw, &sb.path("no-such-kubeconfig")),
            (
                Some(3),
                String::new(),
                "fake kubectl: KUBECONFIG names no file\n".to_string()
            )
        );
        assert_eq!(
            fake_kube::unanswered(&sb),
            ["KUBECONFIG names no file: |get|--raw|/version"]
        );
    }

    #[test]
    fn ssh_answers_by_the_remote_command_and_logs_each_argv() {
        let sb = Sandbox::new();
        FakeSsh::new()
            .answer(fx::node::remote::HOSTNAME, "solo-1\n")
            .fail(fx::node::remote::SWAPFILE_EXISTS, 1, "")
            .install(&sb);
        let kc = kubeconfig(&sb);
        let host = format!("root@{}", fake_ssh::NODE_ADDRESS);
        let hostname = [
            "-o",
            "BatchMode=yes",
            "-i",
            "/home/k",
            host.as_str(),
            "hostname",
        ];
        assert_eq!(
            run(&sb, "ssh", &hostname, &kc),
            (Some(0), "solo-1\n".to_string(), String::new())
        );
        assert_eq!(
            run(
                &sb,
                "ssh",
                &["-i", "/home/k", host.as_str(), "test -e /swapfile"],
                &kc
            ),
            (Some(1), String::new(), String::new())
        );
        assert_eq!(
            run(&sb, "ssh", &[host.as_str(), "uname -r"], &kc),
            (
                Some(255),
                String::new(),
                "fake ssh: no answer for uname -r\n".to_string()
            )
        );
        assert_eq!(
            run(&sb, "ssh", &["root@198.51.100.7", "hostname"], &kc),
            (
                Some(255),
                String::new(),
                "fake ssh: unexpected host root@198.51.100.7\n".to_string()
            )
        );
        assert_eq!(
            fake_ssh::calls(&sb),
            ["hostname", "test -e /swapfile", "uname -r", "hostname"]
        );
        assert_eq!(fake_ssh::argv(&sb)[0], hostname);
        // Critic round, finding 2: a host the fake refuses is recorded with the commands.
        assert_eq!(
            fake_ssh::unanswered(&sb),
            ["uname -r", "unexpected host root@198.51.100.7: hostname"]
        );
    }

    #[test]
    fn ssh_down_refuses_every_connection() {
        let sb = Sandbox::new();
        FakeSsh::down().install(&sb);
        let kc = kubeconfig(&sb);
        let host = format!("root@{}", fake_ssh::NODE_ADDRESS);
        assert_eq!(
            run(&sb, "ssh", &[host.as_str(), "hostname"], &kc),
            (Some(255), String::new(), format!("{}\n", fake_ssh::REFUSED))
        );
        assert_eq!(fake_ssh::calls(&sb), ["hostname"]);
        fake_ssh::assert_all_answered(&sb);
    }
}

// ---------------------------------------------------------------------
// What the leaves read, and the stores they read it through
// ---------------------------------------------------------------------

/// kubectl's arguments for each read, after the flags the fake drops.
const PLATFORMSTACK: [&str; 7] = [
    "get",
    "platformstack",
    "default",
    "-n",
    "apprafter-system",
    "-o",
    "json",
];
const BACKUP_STATUS: [&str; 7] = [
    "get",
    "configmap",
    "apprafter-backup-status",
    "-n",
    "apprafter-system",
    "-o",
    "json",
];
/// The roll-up lists every AppRafter Application with an empty selector (`-l ""`, F4).
const APPLICATIONS: [&str; 7] = [
    "get",
    "application.apprafter.io",
    "-l",
    "",
    "-A",
    "-o",
    "json",
];
const REGISTRATIONS: [&str; 8] = [
    "get",
    "application.argoproj.io",
    "-l",
    "",
    "-n",
    "argocd",
    "-o",
    "json",
];
const PLANS: [&str; 5] = ["get", "migrationplan", "-A", "-o", "json"];

/// The apiserver's words for a PlatformStack it does not have.
const NOT_FOUND_STACK: &str =
    r#"Error from server (NotFound): platformstacks.apprafter.io "default" not found"#;
/// The apiserver's words for RBAC that may not read the PlatformStack (B70's 403 on one kind).
const FORBIDDEN_STACK: &str = r#"Error from server (Forbidden): platformstacks.apprafter.io "default" is forbidden: User "golden" cannot get resource "platformstacks" in API group "apprafter.io" in the namespace "apprafter-system""#;

/// `prod`, the active target: provisioned, its kubeconfig cached under the store's age key.
fn cluster() -> Sandbox {
    let sb = Sandbox::new();
    common::fake_kube::cluster_target(&sb, "prod");
    sb
}

/// [`cluster`], with the CLI default naming `gone`, which is no target (B65).
fn dangling_sandbox() -> Sandbox {
    let sb = cluster();
    sb.seed_pointer("gone");
    sb
}

/// Lets every account write to the store's `targets` folder (WI-475): a store not private.
fn open_the_targets_folder(sb: &Sandbox) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        sb.path("apprafter-config/targets"),
        std::fs::Permissions::from_mode(0o777),
    )
    .expect("open the targets folder");
}

// ---------------------------------------------------------------------
// `status`
// ---------------------------------------------------------------------

mod status {
    use super::*;
    use crate::common::fake_kube::{self, key, FakeKubectl, REFUSED, UNAUTHORIZED};
    use apprafter_core::fixtures::apps::{registration, App};
    use apprafter_core::fixtures::platform::{self, Stack};
    use apprafter_core::fixtures::time::Ago;
    use apprafter_core::fixtures::{list, plans};
    use chrono::{DateTime, Utc};
    use serde_json::Value;

    const FORBIDDEN_REGISTRATIONS: &str = r#"Error from server (Forbidden): applications.argoproj.io is forbidden: User "golden" cannot list resource "applications" in API group "argoproj.io" in the namespace "argocd""#;
    const FORBIDDEN_PLANS: &str = r#"Error from server (Forbidden): migrationplans.apprafter.io is forbidden: User "golden" cannot list resource "migrationplans" in API group "apprafter.io" at the cluster scope"#;
    const NO_BACKUP_RECORD: &str =
        r#"Error from server (NotFound): configmaps "apprafter-backup-status" not found"#;

    /// `status`'s reads, each answered: `stack`, the applications and registrations given, and
    /// the plans.
    fn reads(
        stack: &Value,
        crs: Vec<Value>,
        registrations: Vec<Value>,
        plans: Vec<Value>,
    ) -> FakeKubectl {
        FakeKubectl::new()
            .json(&PLATFORMSTACK, stack)
            .json(&APPLICATIONS, &list(crs))
            .json(&REGISTRATIONS, &list(registrations))
            .json(&PLANS, &list(plans))
    }

    /// A healthy cluster's reads with `stack`: `shop (prod)` and `blog`, each claimed by its
    /// registration and reconciled; no plans.
    fn healthy_with(now: DateTime<Utc>, stack: &Value) -> FakeKubectl {
        let crs = vec![
            App::ready(now, "shop", "shop-prod").json(),
            App::ready(now, "blog", "blog").json(),
        ];
        let registrations = vec![
            registration("shop", Some("prod"), &[("shop", "shop-prod")]),
            registration("blog", None, &[("blog", "blog")]),
        ];
        reads(stack, crs, registrations, vec![])
    }

    /// Backups on and working (`BackupHealthy` True).
    fn backups_working(now: DateTime<Utc>) -> Stack {
        Stack::healthy(now).backups_on().condition(
            "BackupHealthy",
            "True",
            "BackupSucceeded",
            "the last backup Job succeeded",
            Ago::Days(3),
        )
    }

    /// The roll-up's applications: `shop (prod)` with two problems, `blog` held with one more,
    /// `lab/scratch` with a problem no registration claims, `web` reconciled, and `old/gone` on
    /// its way out, a problem on its ledger.
    fn troubled(now: DateTime<Utc>) -> (Vec<Value>, Vec<Value>) {
        let crs = vec![
            App::ready(now, "shop", "shop-prod")
                .problem(
                    "ImagePullBackOff",
                    "image ghcr.io/acme/shop:2026.10 not found",
                    Ago::Minutes(3),
                    4,
                )
                .problem(
                    "CrashLoopBackOff",
                    "container web exited 1",
                    Ago::Minutes(9),
                    1,
                )
                .json(),
            App::ready(now, "blog", "blog")
                .held(
                    "EnvSecretMissing",
                    "secret blog/db-url not found; secret blog/smtp not found",
                    Ago::Hours(3),
                )
                .problem(
                    "ProbeFailed",
                    "readiness probe failed: HTTP 503",
                    Ago::Minutes(5),
                    2,
                )
                .json(),
            App::ready(now, "lab", "scratch")
                .problem(
                    "OOMKilled",
                    "container worker was killed at its memory limit",
                    Ago::Minutes(7),
                    1,
                )
                .json(),
            App::ready(now, "web", "web").json(),
            App::ready(now, "old", "gone")
                .problem(
                    "ImagePullBackOff",
                    "image ghcr.io/acme/gone:1 not found",
                    Ago::Minutes(2),
                    1,
                )
                .deleting()
                .json(),
        ];
        let registrations = vec![
            registration("shop", Some("prod"), &[("shop", "shop-prod")]),
            registration("blog", None, &[("blog", "blog")]),
            registration("web", None, &[("web", "web")]),
        ];
        (crs, registrations)
    }

    /// `status` in `sb` against `fake`, compared with the golden `case`; every call answered.
    fn run_status(sb: &Sandbox, fake: FakeKubectl, case: &str) {
        fake.install(sb);
        sb.golden(case, &["status"]);
        fake_kube::assert_all_answered(sb);
    }

    #[test]
    fn healthy() {
        let now = Utc::now();
        let sb = cluster();
        let stack = backups_working(now)
            .condition(
                "BackupRetention",
                "True",
                "Pruned",
                "the last prune kept 14 snapshots",
                Ago::Days(3),
            )
            .json();
        let fake = healthy_with(now, &stack)
            .json(&BACKUP_STATUS, &platform::backup_status(now, Ago::Hours(3)));
        run_status(&sb, fake, "status/healthy");
        assert_eq!(
            fake_kube::calls(&sb),
            [
                key(&PLATFORMSTACK),
                key(&BACKUP_STATUS),
                key(&APPLICATIONS),
                key(&REGISTRATIONS),
                key(&PLANS)
            ]
        );
    }

    /// Every read refused. Today each section still asks; B9 stops after the first (d5-d2b),
    /// and this case moves then, in its own commit.
    #[test]
    fn unreachable() {
        let sb = cluster();
        run_status(
            &sb,
            FakeKubectl::new().otherwise_fail(1, REFUSED),
            "status/unreachable",
        );
        assert_eq!(
            fake_kube::calls(&sb),
            [
                key(&PLATFORMSTACK),
                key(&APPLICATIONS),
                key(&REGISTRATIONS),
                key(&PLANS)
            ]
        );
    }

    /// The PlatformStack read refused, every other read answered: the sections B9 will stop
    /// reading.
    #[test]
    fn unreachable_then_readable() {
        let now = Utc::now();
        let sb = cluster();
        let fake = healthy_with(now, &Stack::healthy(now).json())
            .fail(&PLATFORMSTACK, 1, REFUSED)
            .json(
                &PLANS,
                &list(vec![plans::plan(
                    now,
                    "shop",
                    "shop-prod-7f2a",
                    Some("destructive"),
                    None,
                )]),
            );
        run_status(&sb, fake, "status/unreachable_then_readable");
    }

    /// Every read 401: the cached credentials no longer open the cluster (B70's `unauthorized`).
    #[test]
    fn unauthorized() {
        let sb = cluster();
        run_status(
            &sb,
            FakeKubectl::new().otherwise_fail(1, UNAUTHORIZED),
            "status/unauthorized",
        );
    }

    /// A 403 on the PlatformStack alone; the other kinds read (B70: that section's `NotRead`).
    #[test]
    fn forbidden_platformstack() {
        let now = Utc::now();
        let sb = cluster();
        let fake =
            healthy_with(now, &Stack::healthy(now).json()).fail(&PLATFORMSTACK, 1, FORBIDDEN_STACK);
        run_status(&sb, fake, "status/forbidden_platformstack");
    }

    #[test]
    fn no_platformstack() {
        let now = Utc::now();
        let sb = cluster();
        let fake =
            healthy_with(now, &Stack::healthy(now).json()).fail(&PLATFORMSTACK, 1, NOT_FOUND_STACK);
        run_status(&sb, fake, "status/no_platformstack");
    }

    /// A target never provisioned: no `hetzner_cloud` in its state. Nothing reads the cluster.
    #[test]
    fn no_server() {
        let sb = Sandbox::new();
        sb.add_target("prod");
        run_status(&sb, FakeKubectl::new(), "status/no_server");
        assert!(fake_kube::calls(&sb).is_empty());
    }

    #[test]
    fn not_cached() {
        let sb = Sandbox::new();
        fake_kube::provisioned_target(&sb, "prod");
        run_status(&sb, FakeKubectl::new(), "status/not_cached");
        assert!(fake_kube::calls(&sb).is_empty());
    }

    /// No `kubectl` on `PATH`: the child can start no real one (B8 (3) makes this
    /// `tool_not_found`).
    #[test]
    fn kubectl_missing() {
        cluster().golden("status/kubectl_missing", &["status"]);
    }

    /// A locked store and no terminal to ask at: the cluster half is refused before any read.
    #[test]
    fn locked_no_terminal() {
        let sb = Sandbox::new();
        fake_kube::locked_cluster_target(&sb, "prod");
        run_status(&sb, FakeKubectl::new(), "status/locked_no_terminal");
        assert!(fake_kube::calls(&sb).is_empty());
    }

    /// A locked store and no `kubectl`: today the store refuses first; B63 says `tool_not_found`
    /// before `store_locked` (B8 (3)).
    #[test]
    fn locked_kubectl_missing() {
        let sb = Sandbox::new();
        fake_kube::locked_cluster_target(&sb, "prod");
        sb.golden("status/locked_kubectl_missing", &["status"]);
    }

    #[test]
    fn upgrade_available() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .available(platform::NEWER)
            .condition(
                "UpgradeAvailable",
                "True",
                "NewerRelease",
                "1.5.0 is available on channel stable",
                Ago::Hours(3),
            )
            .json();
        run_status(&sb, healthy_with(now, &stack), "status/upgrade_available");
    }

    /// The channel's newest release is older than the one running, which is not yanked.
    #[test]
    fn older_release_on_the_channel() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .current(platform::NEWER)
            .available(platform::CURRENT)
            .json();
        run_status(
            &sb,
            healthy_with(now, &stack),
            "status/older_release_on_the_channel",
        );
    }

    #[test]
    fn yanked_with_only_older_on_the_channel() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .current(platform::NEWER)
            .available(platform::CURRENT)
            .condition(
                "YankedVersion",
                "True",
                "Yanked",
                "1.5.0 was yanked: needs.redis is broken in it",
                Ago::Hours(3),
            )
            .json();
        run_status(
            &sb,
            healthy_with(now, &stack),
            "status/yanked_with_only_older_on_the_channel",
        );
    }

    /// Each kind of condition `status` shows: a positive one not True, two negative ones True —
    /// `NodeDiskPressure` and a current `ReconcileStalled` — and a type this CLI never heard of.
    #[test]
    fn unhealthy_conditions() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .condition(
                "Synced",
                "False",
                "OutOfSync",
                "the platform Application is OutOfSync",
                Ago::Hours(3),
            )
            .condition(
                "NodeDiskPressure",
                "True",
                "DiskPressure",
                "node solo-1 reports DiskPressure",
                Ago::Hours(3),
            )
            .condition(
                "ReconcileStalled",
                "True",
                "ReconcileTimedOut",
                "the last reconcile did not finish within 120s and was abandoned",
                Ago::Hours(3),
            )
            .condition(
                "CertificateExpiring",
                "True",
                "Expiring",
                "a condition this CLI has never heard of",
                Ago::Hours(3),
            )
            .json();
        run_status(
            &sb,
            healthy_with(now, &stack),
            "status/unhealthy_conditions",
        );
    }

    /// A `ReconcileStalled` that an operator older than 0.2.81 carried forward: not current.
    #[test]
    fn stall_left_by_a_newer_release() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .current(platform::BEFORE_REPORTING)
            .available(platform::BEFORE_REPORTING)
            .condition(
                "ReconcileStalled",
                "True",
                "ReconcileTimedOut",
                "the last reconcile did not finish within 120s and was abandoned",
                Ago::Hours(3),
            )
            .json();
        run_status(
            &sb,
            healthy_with(now, &stack),
            "status/stall_left_by_a_newer_release",
        );
    }

    #[test]
    fn backups_not_enabled() {
        let now = Utc::now();
        let sb = cluster();
        run_status(
            &sb,
            healthy_with(now, &Stack::healthy(now).json()),
            "status/backups_not_enabled",
        );
    }

    /// Working, and the runner's record cannot be read: the line carries no age.
    #[test]
    fn backups_working_without_a_last_success() {
        let now = Utc::now();
        let sb = cluster();
        let fake = healthy_with(now, &backups_working(now).json()).fail(
            &BACKUP_STATUS,
            1,
            NO_BACKUP_RECORD,
        );
        run_status(&sb, fake, "status/backups_working_without_a_last_success");
    }

    #[test]
    fn backups_failing() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .backups_on()
            .condition(
                "BackupHealthy",
                "False",
                "RunnerOOMKilled",
                "Job apprafter-system/apprafter-backup-29871 was OOMKilled",
                Ago::Days(3),
            )
            .json();
        run_status(&sb, healthy_with(now, &stack), "status/backups_failing");
    }

    #[test]
    fn backups_not_known() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .backups_on()
            .condition(
                "BackupHealthy",
                "Unknown",
                "NoRunYet",
                "no backup Job has run since backups were enabled",
                Ago::Hours(3),
            )
            .json();
        run_status(&sb, healthy_with(now, &stack), "status/backups_not_known");
    }

    /// Backups on, on an operator older than the release that reports on them (0.2.80), with a
    /// `BackupHealthy` a newer one left on the stack.
    #[test]
    fn backups_not_reported_by_an_older_operator() {
        let now = Utc::now();
        let sb = cluster();
        let stack = backups_working(now)
            .current(platform::BEFORE_REPORTING)
            .available(platform::BEFORE_REPORTING)
            .json();
        run_status(
            &sb,
            healthy_with(now, &stack),
            "status/backups_not_reported_by_an_older_operator",
        );
    }

    #[test]
    fn backups_not_reported() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now).backups_on().json();
        run_status(
            &sb,
            healthy_with(now, &stack),
            "status/backups_not_reported",
        );
    }

    #[test]
    fn retention_not_enforced() {
        let now = Utc::now();
        let sb = cluster();
        let stack = backups_working(now)
            .condition(
                "BackupRetention",
                "False",
                "PruneFailed",
                "restic forget --prune exited 1",
                Ago::Days(3),
            )
            .json();
        let fake = healthy_with(now, &stack)
            .json(&BACKUP_STATUS, &platform::backup_status(now, Ago::Hours(3)));
        run_status(&sb, fake, "status/retention_not_enforced");
    }

    /// Enforced from outside the cluster, by choice, and the last prune ran 12 days ago (past
    /// the 8-day grace).
    #[test]
    fn retention_pruned_from_outside_long_ago() {
        let now = Utc::now();
        let sb = cluster();
        let stack = backups_working(now)
            .condition(
                "BackupRetention",
                "False",
                "EnforcedOutsideCluster",
                "spec.backup.retention.enforce is operator",
                Ago::Days(12),
            )
            .last_prune(Ago::Days(12))
            .json();
        let fake = healthy_with(now, &stack)
            .json(&BACKUP_STATUS, &platform::backup_status(now, Ago::Hours(3)));
        run_status(&sb, fake, "status/retention_pruned_from_outside_long_ago");
    }

    #[test]
    fn retention_not_judged_for_days() {
        let now = Utc::now();
        let sb = cluster();
        let stack = backups_working(now)
            .condition(
                "BackupRetention",
                "Unknown",
                "NoCheckYet",
                "no weekly check has finished",
                Ago::Days(12),
            )
            .json();
        let fake = healthy_with(now, &stack)
            .json(&BACKUP_STATUS, &platform::backup_status(now, Ago::Hours(3)));
        run_status(&sb, fake, "status/retention_not_judged_for_days");
    }

    #[test]
    fn problem_apps() {
        let now = Utc::now();
        let sb = cluster();
        let (crs, registrations) = troubled(now);
        run_status(
            &sb,
            reads(&Stack::healthy(now).json(), crs, registrations, vec![]),
            "status/problem_apps",
        );
    }

    /// Eleven applications with a problem each, listed newest last so the order printed is the
    /// sort's; `app-03` and `app-04` were last seen at the same second, so only the tie-break
    /// (the rendered row) orders them (F10).
    #[test]
    fn problem_apps_cap() {
        let now = Utc::now();
        let sb = cluster();
        let names: Vec<String> = (1..=11).map(|n| format!("app-{n:02}")).collect();
        let minutes = |n: usize| -> u8 {
            if n == 4 {
                3
            } else {
                n as u8
            }
        };
        let crs: Vec<Value> = names
            .iter()
            .enumerate()
            .rev()
            .map(|(i, name)| {
                App::ready(now, "apps", name)
                    .problem(
                        "CrashLoopBackOff",
                        "container main exited 1",
                        Ago::Minutes(minutes(i + 1)),
                        1,
                    )
                    .json()
            })
            .collect();
        let registrations: Vec<Value> = names
            .iter()
            .map(|name| registration(name, None, &[("apps", name.as_str())]))
            .collect();
        run_status(
            &sb,
            reads(&Stack::healthy(now).json(), crs, registrations, vec![]),
            "status/problem_apps_cap",
        );
    }

    /// The registrations read refused: rows are named by their CR, and a note says why.
    #[test]
    fn problem_apps_naming_failed() {
        let now = Utc::now();
        let sb = cluster();
        let (crs, registrations) = troubled(now);
        let fake = reads(&Stack::healthy(now).json(), crs, registrations, vec![]).fail(
            &REGISTRATIONS,
            1,
            FORBIDDEN_REGISTRATIONS,
        );
        run_status(&sb, fake, "status/problem_apps_naming_failed");
    }

    #[test]
    fn pinned_apps() {
        let now = Utc::now();
        let sb = cluster();
        let crs = vec![
            App::ready(now, "web", "web")
                .pinned("ghcr.io/acme/web@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .json(),
            App::ready(now, "api", "api")
                .pinned("ghcr.io/acme/api@sha256:fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
                .json(),
        ];
        let registrations = vec![
            registration("web", None, &[("web", "web")]),
            registration("api", None, &[("api", "api")]),
        ];
        run_status(
            &sb,
            reads(&Stack::healthy(now).json(), crs, registrations, vec![]),
            "status/pinned_apps",
        );
    }

    /// Two plans waiting (one with a classification, one without) and one decided.
    #[test]
    fn plans_pending() {
        let now = Utc::now();
        let sb = cluster();
        let waiting = vec![
            plans::plan(now, "shop", "shop-prod-7f2a", Some("destructive"), None),
            plans::plan(
                now,
                "apprafter-system",
                "platform-1-5-0",
                None,
                Some("pending-approval"),
            ),
            plans::plan(
                now,
                "blog",
                "blog-3c1d",
                Some("security-boundary"),
                Some("approved"),
            ),
        ];
        let fake = healthy_with(now, &Stack::healthy(now).json()).json(&PLANS, &list(waiting));
        run_status(&sb, fake, "status/plans_pending");
    }

    #[test]
    fn plans_unreadable() {
        let now = Utc::now();
        let sb = cluster();
        let fake = healthy_with(now, &Stack::healthy(now).json()).fail(&PLANS, 1, FORBIDDEN_PLANS);
        run_status(&sb, fake, "status/plans_unreadable");
    }

    /// A store other accounts can change (WI-475): the local half prints (WI-481), the cluster
    /// half is refused before the state is read — with a server recorded, and without one.
    #[test]
    fn store_not_private() {
        let sb = cluster();
        open_the_targets_folder(&sb);
        run_status(&sb, FakeKubectl::new(), "status/store_not_private");
        assert!(fake_kube::calls(&sb).is_empty());
    }

    #[test]
    fn store_not_private_no_server() {
        let sb = Sandbox::new();
        sb.add_target("prod");
        open_the_targets_folder(&sb);
        run_status(
            &sb,
            FakeKubectl::new(),
            "status/store_not_private_no_server",
        );
    }

    #[test]
    fn age_key_missing() {
        let sb = Sandbox::new();
        fake_kube::cluster_target_key_lost(&sb, "prod");
        run_status(&sb, FakeKubectl::new(), "status/age_key_missing");
        assert!(fake_kube::calls(&sb).is_empty());
    }

    #[test]
    fn cache_undecryptable() {
        let sb = Sandbox::new();
        fake_kube::cluster_target_other_key(&sb, "prod");
        run_status(&sb, FakeKubectl::new(), "status/cache_undecryptable");
        assert!(fake_kube::calls(&sb).is_empty());
    }

    /// The CLI default names a target that is gone (B65): today the missing target's header and
    /// "run `apprafter apply`", exit 0; B8 (5) refuses it with `target_not_found`.
    #[test]
    fn dangling() {
        let sb = dangling_sandbox();
        run_status(&sb, FakeKubectl::new(), "status/dangling");
        assert!(fake_kube::calls(&sb).is_empty());
    }
}

// ---------------------------------------------------------------------
// `top`
// ---------------------------------------------------------------------

mod top {
    use super::*;
    use crate::common::fake_kube::{self, key, FakeKubectl, REFUSED};
    use apprafter_core::fixtures::list;
    use apprafter_core::fixtures::top::{
        node_metrics, other_pod, pending_pod, pod_metrics, solo_nodes, solo_pods, summary,
    };
    use serde_json::Value;

    const NODES: [&str; 4] = ["get", "nodes", "-o", "json"];
    const PODS: [&str; 5] = ["get", "pods", "-A", "-o", "json"];
    const NODE_METRICS: [&str; 3] = ["get", "--raw", "/apis/metrics.k8s.io/v1beta1/nodes"];
    const POD_METRICS: [&str; 3] = ["get", "--raw", "/apis/metrics.k8s.io/v1beta1/pods"];
    const SUMMARY: [&str; 3] = ["get", "--raw", "/api/v1/nodes/solo-1/proxy/stats/summary"];

    const METRICS_NOT_SERVED: &str =
        "Error from server (NotFound): the server could not find the requested resource";
    const SUMMARY_UNAVAILABLE: &str = "Error from server (ServiceUnavailable): the server is currently unable to handle the request";
    /// B72: a first line over 300 characters, which `top` prints whole.
    const LONG_SUMMARY_ERROR: &str = r#"Error from server (InternalError): an error on the server ("proxy error from 127.0.0.1:6443 while dialing 10.0.0.2:10250, code 502: 502 Bad Gateway") has prevented the request from succeeding (get nodes solo-1:10250 proxy stats summary): the kubelet on solo-1 closed the connection before it sent a response, which it does while it restarts after a configuration change"#;

    /// `top`'s reads of the one-node cluster running `pods`, each answered.
    fn reads(pods: Vec<Value>) -> FakeKubectl {
        FakeKubectl::new()
            .json(&NODES, &solo_nodes())
            .json(&PODS, &list(pods.clone()))
            .json(&NODE_METRICS, &node_metrics())
            .json(&POD_METRICS, &pod_metrics(&pods))
            .json(&SUMMARY, &summary(&pods))
    }

    fn run_top(sb: &Sandbox, fake: FakeKubectl, case: &str) {
        fake.install(sb);
        sb.golden(case, &["top"]);
        fake_kube::assert_all_answered(sb);
    }

    #[test]
    fn full() {
        let sb = cluster();
        run_top(&sb, reads(solo_pods()), "top/full");
        assert_eq!(
            fake_kube::calls(&sb),
            [
                key(&NODES),
                key(&PODS),
                key(&NODE_METRICS),
                key(&POD_METRICS),
                key(&SUMMARY)
            ]
        );
    }

    /// No metrics-server: both metrics reads fail (F9), the measured columns are `—`.
    #[test]
    fn metrics_absent() {
        let sb = cluster();
        let fake = reads(solo_pods())
            .fail(&NODE_METRICS, 1, METRICS_NOT_SERVED)
            .fail(&POD_METRICS, 1, METRICS_NOT_SERVED);
        run_top(&sb, fake, "top/metrics_absent");
    }

    #[test]
    fn summary_failing() {
        let sb = cluster();
        run_top(
            &sb,
            reads(solo_pods()).fail(&SUMMARY, 1, SUMMARY_UNAVAILABLE),
            "top/summary_failing",
        );
    }

    #[test]
    fn summary_failing_long() {
        assert!(
            LONG_SUMMARY_ERROR.chars().count() > 300,
            "B72 wants a line over 300 characters"
        );
        let sb = cluster();
        run_top(
            &sb,
            reads(solo_pods()).fail(&SUMMARY, 1, LONG_SUMMARY_ERROR),
            "top/summary_failing_long",
        );
    }

    /// A failure with an empty stderr: `top`'s own words for it (B72 keeps them).
    #[test]
    fn summary_failing_silently() {
        let sb = cluster();
        run_top(
            &sb,
            reads(solo_pods()).fail(&SUMMARY, 1, ""),
            "top/summary_failing_silently",
        );
    }

    #[test]
    fn unscheduled() {
        let sb = cluster();
        let mut pods = solo_pods();
        pods.push(pending_pod());
        run_top(&sb, reads(pods), "top/unscheduled");
    }

    #[test]
    fn other_namespace() {
        let sb = cluster();
        let mut pods = solo_pods();
        pods.push(other_pod());
        run_top(&sb, reads(pods), "top/other_namespace");
    }

    #[test]
    fn no_nodes() {
        let sb = cluster();
        run_top(
            &sb,
            FakeKubectl::new().json(&NODES, &list(vec![])),
            "top/no_nodes",
        );
        assert_eq!(fake_kube::calls(&sb), [key(&NODES)]);
    }

    #[test]
    fn unreachable() {
        let sb = cluster();
        run_top(
            &sb,
            FakeKubectl::new().otherwise_fail(1, REFUSED),
            "top/unreachable",
        );
        assert_eq!(fake_kube::calls(&sb), [key(&NODES)]);
    }

    /// B8 (3) makes this `tool_not_found`.
    #[test]
    fn kubectl_missing() {
        cluster().golden("top/kubectl_missing", &["top"]);
    }

    /// B65: today the misleading "run `apprafter apply`"; B8 (5) refuses with
    /// `target_not_found`.
    #[test]
    fn dangling() {
        let sb = dangling_sandbox();
        run_top(&sb, FakeKubectl::new(), "top/dangling");
        assert!(fake_kube::calls(&sb).is_empty());
    }
}

// ---------------------------------------------------------------------
// `platform status`, `platform autoscale show`, `platform env show`
// ---------------------------------------------------------------------

mod platform {
    use super::*;
    use crate::common::fake_kube::{self, key, FakeKubectl, REFUSED};
    use apprafter_core::fixtures::platform::{Stack, BEFORE_REPORTING, CURRENT, NEWER, OLDER};
    use apprafter_core::fixtures::time::Ago;
    use chrono::{DateTime, Utc};
    use serde_json::Value;

    /// The re-check's merge-patch; its body carries the request's time, so the fake answers any.
    const RECHECK: [&str; 7] = [
        "patch",
        "platformstack",
        "default",
        "-n",
        "apprafter-system",
        "--type=merge",
        "-p",
    ];
    const CACHED: [&str; 3] = ["platform", "status", "--cached"];
    const BOTH_FORMS: [&[&str]; 2] = [&["platform", "status", "--cached"], &["platform", "status"]];
    const DEFAULTS: [&[&str]; 2] = [
        &["platform", "autoscale", "show"],
        &["platform", "env", "show"],
    ];
    const LONG_MESSAGE: &str = "the platform Application apprafter-system/platform reports Degraded: Deployment cert-manager/cert-manager-webhook has 0 of 1 replicas available after 600s, and the readiness probe of its pod cert-manager-webhook-7c9f4d6b5-x2k8q fails with HTTP 500 on /healthz";

    /// A cluster whose one read, the PlatformStack, answers `stack`.
    fn stack_only(stack: &Value) -> FakeKubectl {
        FakeKubectl::new().json(&PLATFORMSTACK, stack)
    }

    /// `args` in `sb` against `fake`, compared with the golden `case`; every call answered.
    fn run_platform(sb: &Sandbox, fake: FakeKubectl, case: &str, args: &[&str]) {
        fake.install(sb);
        sb.golden(case, args);
        fake_kube::assert_all_answered(sb);
    }

    /// Every section filled: an upgrade waiting, backups on and working, retention enforced,
    /// three component versions and a frozen one (which `platform status` does not print yet:
    /// d5-c's Components section, B13, moves this case), two history rows.
    fn full_stack(now: DateTime<Utc>) -> Value {
        Stack::healthy(now)
            .available(NEWER)
            .condition(
                "UpgradeAvailable",
                "True",
                "NewerRelease",
                "1.5.0 is available on channel stable",
                Ago::Hours(3),
            )
            .backups_on()
            .condition(
                "BackupHealthy",
                "True",
                "BackupSucceeded",
                "the last backup Job succeeded",
                Ago::Days(3),
            )
            .condition(
                "BackupRetention",
                "True",
                "Pruned",
                "the last prune kept 14 snapshots",
                Ago::Days(3),
            )
            .components(
                &[
                    ("argocd", "7.7.7"),
                    ("cert-manager", "1.16.2"),
                    ("apprafter-operator", "0.2.51"),
                ],
                &[("cert-manager", "1.16.2")],
            )
            .json()
    }

    #[test]
    fn status_cached_full() {
        let now = Utc::now();
        let sb = cluster();
        run_platform(
            &sb,
            stack_only(&full_stack(now)),
            "platform/status_cached_full",
            &CACHED,
        );
        assert_eq!(fake_kube::calls(&sb), [key(&PLATFORMSTACK)]);
    }

    #[test]
    fn status_cached_without_status() {
        let now = Utc::now();
        let sb = cluster();
        run_platform(
            &sb,
            stack_only(&Stack::without_status(now).json()),
            "platform/status_cached_without_status",
            &CACHED,
        );
    }

    /// Backup rows an operator older than 0.2.80 carried forward: marked not current.
    #[test]
    fn status_cached_backup_rows_not_current() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .current(BEFORE_REPORTING)
            .available(BEFORE_REPORTING)
            .backups_on()
            .condition(
                "BackupHealthy",
                "True",
                "BackupSucceeded",
                "the last backup Job succeeded",
                Ago::Days(3),
            )
            .condition(
                "BackupRetention",
                "True",
                "Pruned",
                "the last prune kept 14 snapshots",
                Ago::Days(3),
            )
            .json();
        run_platform(
            &sb,
            stack_only(&stack),
            "platform/status_cached_backup_rows_not_current",
            &CACHED,
        );
    }

    #[test]
    fn status_cached_stall_not_current() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .current(BEFORE_REPORTING)
            .available(BEFORE_REPORTING)
            .condition(
                "ReconcileStalled",
                "True",
                "ReconcileTimedOut",
                "the last reconcile did not finish within 120s and was abandoned",
                Ago::Hours(3),
            )
            .json();
        run_platform(
            &sb,
            stack_only(&stack),
            "platform/status_cached_stall_not_current",
            &CACHED,
        );
    }

    /// Messages longer than the table's room: wrapped to 100 columns off a terminal.
    #[test]
    fn status_cached_long_messages() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .condition("Ready", "False", "Degraded", LONG_MESSAGE, Ago::Hours(3))
            .condition(
                "Synced",
                "False",
                "OutOfSync",
                &format!("{LONG_MESSAGE}; and the sync of revision 4f1c9e2 failed"),
                Ago::Hours(3),
            )
            .json();
        run_platform(
            &sb,
            stack_only(&stack),
            "platform/status_cached_long_messages",
            &CACHED,
        );
    }

    /// Seven history entries, one without `appliedAt`: the newest five show.
    #[test]
    fn status_cached_history() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .history(&[
                (CURRENT, Some(Ago::Days(3)), "succeeded"),
                (OLDER, Some(Ago::Days(5)), "succeeded"),
                ("1.3.6", Some(Ago::Days(8)), "rolled-back"),
                ("1.3.5", Some(Ago::Days(12)), "succeeded"),
                ("1.3.4", Some(Ago::Days(16)), "succeeded"),
                ("1.3.3", Some(Ago::Days(20)), "succeeded"),
                ("1.3.2", None, "succeeded"),
            ])
            .json();
        run_platform(
            &sb,
            stack_only(&stack),
            "platform/status_cached_history",
            &CACHED,
        );
    }

    #[test]
    fn status_cached_not_found() {
        let sb = cluster();
        run_platform(
            &sb,
            FakeKubectl::new().fail(&PLATFORMSTACK, 1, NOT_FOUND_STACK),
            "platform/status_cached_not_found",
            &CACHED,
        );
    }

    #[test]
    fn status_cached_unreachable() {
        let sb = cluster();
        run_platform(
            &sb,
            FakeKubectl::new().otherwise_fail(1, REFUSED),
            "platform/status_cached_unreachable",
            &CACHED,
        );
    }

    /// Without `--cached`: the merge-patch asks for a re-check, and the operator's answer is a
    /// `lastUpstreamCheck` 1 h 30 min ahead of the run's clock (B76), later than the request,
    /// so the first poll ends it.
    #[test]
    fn status_recheck() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now)
            .last_upstream_check(Ago::HourAndAHalf.after(now))
            .json();
        let fake = FakeKubectl::new()
            .ok_prefix(&RECHECK)
            .json(&PLATFORMSTACK, &stack);
        run_platform(
            &sb,
            fake,
            "platform/status_recheck",
            &["platform", "status"],
        );
        let calls = fake_kube::calls(&sb);
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls[0].starts_with(&key(&RECHECK)), "{calls:?}");
        assert!(
            calls[0].contains("apprafter.io/recheck-requested"),
            "{calls:?}"
        );
        assert_eq!(&calls[1..], &[key(&PLATFORMSTACK), key(&PLATFORMSTACK)][..]);
    }

    #[test]
    fn status_kubectl_missing() {
        cluster().golden_steps("platform/status_kubectl_missing", &BOTH_FORMS);
    }

    #[test]
    fn status_dangling() {
        let sb = dangling_sandbox();
        FakeKubectl::new().install(&sb);
        sb.golden_steps("platform/status_dangling", &BOTH_FORMS);
        assert!(fake_kube::calls(&sb).is_empty());
    }

    #[test]
    fn autoscale_show_set() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now).autoscale("up-only").json();
        run_platform(
            &sb,
            stack_only(&stack),
            "platform/autoscale_show_set",
            DEFAULTS[0],
        );
    }

    #[test]
    fn autoscale_show_unset() {
        let now = Utc::now();
        let sb = cluster();
        run_platform(
            &sb,
            stack_only(&Stack::healthy(now).json()),
            "platform/autoscale_show_unset",
            DEFAULTS[0],
        );
    }

    /// No PlatformStack: today the default, no error (F6).
    #[test]
    fn autoscale_show_no_platformstack() {
        let sb = cluster();
        let fake = FakeKubectl::new().fail(&PLATFORMSTACK, 1, NOT_FOUND_STACK);
        run_platform(
            &sb,
            fake,
            "platform/autoscale_show_no_platformstack",
            DEFAULTS[0],
        );
    }

    #[test]
    fn env_show_set() {
        let now = Utc::now();
        let sb = cluster();
        let stack = Stack::healthy(now).default_environment("prod").json();
        run_platform(
            &sb,
            stack_only(&stack),
            "platform/env_show_set",
            DEFAULTS[1],
        );
    }

    #[test]
    fn env_show_unset() {
        let now = Utc::now();
        let sb = cluster();
        run_platform(
            &sb,
            stack_only(&Stack::healthy(now).json()),
            "platform/env_show_unset",
            DEFAULTS[1],
        );
    }

    /// No PlatformStack: today `(unset)`, no error (F6).
    #[test]
    fn env_show_no_platformstack() {
        let sb = cluster();
        let fake = FakeKubectl::new().fail(&PLATFORMSTACK, 1, NOT_FOUND_STACK);
        run_platform(&sb, fake, "platform/env_show_no_platformstack", DEFAULTS[1]);
    }

    #[test]
    fn defaults_unreachable() {
        let sb = cluster();
        FakeKubectl::new().otherwise_fail(1, REFUSED).install(&sb);
        sb.golden_steps("platform/defaults_unreachable", &DEFAULTS);
    }

    #[test]
    fn defaults_kubectl_missing() {
        cluster().golden_steps("platform/defaults_kubectl_missing", &DEFAULTS);
    }

    #[test]
    fn defaults_dangling() {
        let sb = dangling_sandbox();
        FakeKubectl::new().install(&sb);
        sb.golden_steps("platform/defaults_dangling", &DEFAULTS);
        assert!(fake_kube::calls(&sb).is_empty());
    }
}

// ---------------------------------------------------------------------
// `node status`
// ---------------------------------------------------------------------

mod node {
    use super::*;
    use crate::common::fake_kube::{self, key, FakeKubectl, REFUSED};
    use crate::common::fake_ssh::{self, FakeSsh};
    use apprafter_core::fixtures::node::{self as fxn, remote};

    const NODE_NAME: [&str; 4] = ["get", "nodes", "-o", "jsonpath={.items[0].metadata.name}"];
    const CONFIGZ: [&str; 3] = ["get", "--raw", "/api/v1/nodes/solo-1/proxy/configz"];
    const SWAP_CAPACITY: [&str; 5] = [
        "get",
        "node",
        "solo-1",
        "-o",
        "jsonpath={.status.nodeInfo.swap.capacity}",
    ];
    const KUBELET_VERSION: [&str; 5] = [
        "get",
        "node",
        "solo-1",
        "-o",
        "jsonpath={.status.nodeInfo.kubeletVersion}",
    ];

    /// `GET /v1/servers` (today's address read, B66): server 42 at 203.0.113.10.
    const SERVERS_42: &str = r#"{"servers":[{"id":42,"name":"prod-node","status":"running","labels":{"apprafter":"true"},"public_net":{"ipv4":{"ip":"203.0.113.10"},"ipv6":{"ip":"2001:db8:1::/64"}}}]}"#;
    /// Server 42 with no public IPv4 yet.
    const SERVERS_42_WITHOUT_IPV4: &str = r#"{"servers":[{"id":42,"name":"prod-node","status":"initializing","labels":{"apprafter":"true"},"public_net":{"ipv4":null,"ipv6":{"ip":"2001:db8:1::/64"}}}]}"#;
    /// Server 42 deleted at the provider: only another target's server is listed (B66).
    const SERVERS_WITHOUT_42: &str = r#"{"servers":[{"id":43,"name":"staging-node","status":"running","labels":{"apprafter":"true"},"public_net":{"ipv4":{"ip":"203.0.113.11"}}}]}"#;
    const NO_BREADCRUMB: &str =
        "cat: /var/lib/apprafter/swap-provision.status: No such file or directory";

    /// The node's kube API reads, each answered: swap applied on a 1.35 kubelet.
    fn kube_up() -> FakeKubectl {
        FakeKubectl::new()
            .text(&NODE_NAME, fxn::NODE)
            .text(&CONFIGZ, &fxn::configz(false, Some("NoSwap")))
            .text(&SWAP_CAPACITY, fxn::SWAP_CAPACITY)
            .text(&KUBELET_VERSION, fxn::KUBELET_VERSION)
    }

    /// The seven probes of a node whose swap is active.
    fn ssh_swap_active() -> FakeSsh {
        FakeSsh::new()
            .answer(remote::HOSTNAME, "solo-1\n")
            .answer(remote::SWAPON, fxn::SWAPON_ACTIVE)
            .answer(remote::FSTAB, fxn::FSTAB_WITH_SWAP)
            .answer(remote::SWAPFILE_EXISTS, "")
            .answer(remote::SWAPPINESS, fxn::SWAPPINESS)
            .answer(remote::GOMEMLIMIT, fxn::K3S_ENVIRONMENT)
            .answer(remote::BREADCRUMB, fxn::BREADCRUMB_APPLIED)
    }

    /// The provider lists `servers` (`hits` times, which the caller asserts); `prod` is
    /// provisioned with its kubeconfig cached; the environment also holds `env`.
    fn sandbox(
        server: &mut mockito::Server,
        servers: &str,
        hits: usize,
        env: &[(&str, &str)],
    ) -> (Sandbox, mockito::Mock) {
        let route = json_route(server, "/v1/servers", 200, servers, TOKEN_A)
            .expect(hits)
            .create();
        let mut sb = Sandbox::new().with_hcloud(server.url());
        for (name, value) in env {
            sb = sb.with_env(name, value);
        }
        fake_kube::cluster_target(&sb, "prod");
        (sb, route)
    }

    /// `node status` in `sb` against the two fakes, compared with the golden `case`; every call
    /// answered.
    fn run_node(sb: &Sandbox, kube: FakeKubectl, ssh: FakeSsh, case: &str) {
        kube.install(sb);
        ssh.install(sb);
        sb.golden(case, &["node", "status"]);
        fake_kube::assert_all_answered(sb);
        fake_ssh::assert_all_answered(sb);
    }

    #[test]
    fn status_swap_active() {
        let mut server = mockito::Server::new();
        let (sb, route) = sandbox(&mut server, SERVERS_42, 1, &[]);
        run_node(&sb, kube_up(), ssh_swap_active(), "node/status_swap_active");
        route.assert();
        assert_eq!(
            fake_kube::calls(&sb),
            [
                key(&NODE_NAME),
                key(&CONFIGZ),
                key(&SWAP_CAPACITY),
                key(&KUBELET_VERSION)
            ]
        );
        assert_eq!(fake_ssh::calls(&sb), remote::ALL);
        // B68's baseline: the CLI's ssh argv, which d5-f keeps exactly.
        let first: Vec<String> = fake_ssh::argv(&sb)[0]
            .iter()
            .map(|a| sb.normalize(a))
            .collect();
        assert_eq!(
            first,
            [
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "UserKnownHostsFile=<SANDBOX>/apprafter-config/state/prod/.apprafter/known_hosts",
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "ConnectTimeout=5",
                "-i",
                "<SANDBOX>/home/.ssh/id_ed25519",
                "root@203.0.113.10",
                "hostname",
            ]
        );
    }

    #[test]
    fn status_ssh_down() {
        let mut server = mockito::Server::new();
        let (sb, route) = sandbox(&mut server, SERVERS_42, 1, &[]);
        run_node(&sb, kube_up(), FakeSsh::down(), "node/status_ssh_down");
        route.assert();
        assert_eq!(fake_ssh::calls(&sb), remote::ALL);
    }

    /// The kube API refuses, SSH answers: the `[api]` group alone is unknown (B16).
    #[test]
    fn status_api_down() {
        let mut server = mockito::Server::new();
        let (sb, route) = sandbox(&mut server, SERVERS_42, 1, &[]);
        run_node(
            &sb,
            FakeKubectl::new().otherwise_fail(1, REFUSED),
            ssh_swap_active(),
            "node/status_api_down",
        );
        route.assert();
        assert_eq!(fake_kube::calls(&sb), [key(&NODE_NAME)]);
    }

    #[test]
    fn status_no_ipv4() {
        let mut server = mockito::Server::new();
        let (sb, route) = sandbox(&mut server, SERVERS_42_WITHOUT_IPV4, 1, &[]);
        run_node(
            &sb,
            FakeKubectl::new(),
            FakeSsh::new(),
            "node/status_no_ipv4",
        );
        route.assert();
        assert!(fake_kube::calls(&sb).is_empty() && fake_ssh::calls(&sb).is_empty());
    }

    /// The server deleted at the provider: today the no-IPv4 words; B8 (6) says
    /// `server_missing` (d5-f).
    #[test]
    fn status_server_deleted() {
        let mut server = mockito::Server::new();
        let (sb, route) = sandbox(&mut server, SERVERS_WITHOUT_42, 1, &[]);
        run_node(
            &sb,
            FakeKubectl::new(),
            FakeSsh::new(),
            "node/status_server_deleted",
        );
        route.assert();
        assert!(fake_kube::calls(&sb).is_empty() && fake_ssh::calls(&sb).is_empty());
    }

    /// `APPRAFTER_SKIP_NODE_SWAP` set, no swap on the node: the skipped verdict (the CLI reads
    /// the variable; d5-f moves the read to its builder).
    #[test]
    fn status_skip_node_swap() {
        let mut server = mockito::Server::new();
        let (sb, route) = sandbox(
            &mut server,
            SERVERS_42,
            1,
            &[("APPRAFTER_SKIP_NODE_SWAP", "1")],
        );
        let kube = kube_up()
            .text(&CONFIGZ, &fxn::configz(true, None))
            .text(&SWAP_CAPACITY, "");
        let ssh = FakeSsh::new()
            .answer(remote::HOSTNAME, "solo-1\n")
            .answer(remote::SWAPON, "")
            .answer(remote::FSTAB, fxn::FSTAB_WITHOUT_SWAP)
            .fail(remote::SWAPFILE_EXISTS, 1, "")
            .answer(remote::SWAPPINESS, "60\n")
            .answer(remote::GOMEMLIMIT, fxn::K3S_ENVIRONMENT_WITHOUT_GOMEMLIMIT)
            .fail(remote::BREADCRUMB, 1, NO_BREADCRUMB);
        run_node(&sb, kube, ssh, "node/status_skip_node_swap");
        route.assert();
    }

    /// A target never provisioned: refused before the token or the provider.
    #[test]
    fn status_no_server() {
        let mut server = mockito::Server::new();
        let route = json_route(&mut server, "/v1/servers", 200, SERVERS_42, TOKEN_A)
            .expect(0)
            .create();
        let sb = Sandbox::new().with_hcloud(server.url());
        sb.add_target("prod");
        run_node(
            &sb,
            FakeKubectl::new(),
            FakeSsh::new(),
            "node/status_no_server",
        );
        route.assert();
    }

    /// No kubeconfig cached: the `[api]` group unknown, the `[ssh]` group read.
    #[test]
    fn status_not_cached() {
        let mut server = mockito::Server::new();
        let route = json_route(&mut server, "/v1/servers", 200, SERVERS_42, TOKEN_A)
            .expect(1)
            .create();
        let sb = Sandbox::new().with_hcloud(server.url());
        fake_kube::provisioned_target(&sb, "prod");
        run_node(
            &sb,
            FakeKubectl::new(),
            ssh_swap_active(),
            "node/status_not_cached",
        );
        route.assert();
        assert!(fake_kube::calls(&sb).is_empty());
    }

    /// No `kubectl` on `PATH`, SSH answers: the `[api]` group unknown.
    #[test]
    fn status_kubectl_missing() {
        let mut server = mockito::Server::new();
        let (sb, route) = sandbox(&mut server, SERVERS_42, 1, &[]);
        ssh_swap_active().install(&sb);
        sb.golden("node/status_kubectl_missing", &["node", "status"]);
        fake_ssh::assert_all_answered(&sb);
        route.assert();
    }

    #[test]
    fn status_dangling() {
        let mut server = mockito::Server::new();
        let route = json_route(&mut server, "/v1/servers", 200, SERVERS_42, TOKEN_A)
            .expect(0)
            .create();
        let sb = Sandbox::new().with_hcloud(server.url());
        fake_kube::cluster_target(&sb, "prod");
        sb.seed_pointer("gone");
        run_node(
            &sb,
            FakeKubectl::new(),
            FakeSsh::new(),
            "node/status_dangling",
        );
        route.assert();
        assert!(fake_kube::calls(&sb).is_empty() && fake_ssh::calls(&sb).is_empty());
    }
}
