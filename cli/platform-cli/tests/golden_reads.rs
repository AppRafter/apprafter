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
