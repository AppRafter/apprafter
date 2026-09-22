// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Real-cluster proof that the runner's pod-exec path ENDS: a `pg_dump` behind
//! a held table lock fails within its `--lock-wait-timeout` with the lock named
//! in the error; a `pg_dump` behind a held MATERIALIZED VIEW lock — which that
//! flag does not cover — fails at its first-output bound with the lock
//! explained; and a command that writes more stderr than kube-rs's 1 KiB pipe
//! still returns. All three hung on kube-rs 4 before the fixes these tests
//! came with, and none shows up against a stub: the locks are PostgreSQL's,
//! and the pipe sits inside kube-rs's WebSocket message loop.
//!
//! Skipped by default. Opt in against a DISPOSABLE kind cluster:
//!
//! ```text
//! APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> \
//!     cargo test -p apprafter-backup --test exec_hang_kind_test -- --ignored
//! ```
//!
//! Needs `docker.io/library/postgres:18-alpine` on the node (pulled
//! `IfNotPresent`, so it can be preloaded) — the major the platform's CNPG
//! operand runs, and so the `pg_dump` the runner picks for it
//! (`images::pg_helper_image`). A plain PostgreSQL pod stands in for CNPG:
//! the wait under test is between `pg_dump` and the server's lock manager,
//! both of which are stock PostgreSQL in the CNPG image, and the path to it is
//! the same helper pod → Service → server hop the runner takes.
//!
//! The lock tests wait out the real bounds — five minutes for the table lock,
//! ten for the first output — so they take about six and twelve minutes. Each
//! wait is guarded by a watchdog, so a regression FAILS instead of hanging the
//! run. Refuses any kubeconfig whose current context is not a `kind-*`
//! context.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apprafter_backup::kube_rs_exec::KubeRsExec;
use backup_core::extract::{
    run_extraction, ExtractItem, PG_DUMP_FIRST_OUTPUT_WITHIN, PG_DUMP_LOCK_WAIT_TIMEOUT,
};
use backup_core::{DataKind, KubeExec};
use k8s_openapi::api::core::v1::{Namespace, Secret, Service};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use serde_json::json;

const MANAGER: &str = "apprafter-exec-hang-kind";
const PG_IMAGE: &str = "docker.io/library/postgres:18-alpine";

/// The lock-wait bound the argv carries, in seconds.
const LOCK_WAIT: Duration = Duration::from_secs(300);
/// What a run may add on top of the lock wait: the helper pod's start, the
/// dump's connect and catalog reads, the exec round trips.
const SETUP_SLACK: Duration = Duration::from_secs(120);

/// Prefix of a lock-holding pod's script: wait until the server answers
/// through its Service. The Service is created once the server is Ready, and
/// its endpoints reach the node's forwarding rules a moment later — a `psql`
/// started in that gap fails, and the pod's restart backoff then outlasts the
/// test's wait for the lock.
const WAIT_FOR_SERVICE: &str = "until pg_isready -q -h pg -U postgres; do sleep 1; done;";

/// Tables in the dumped database. pg_dump 18 names every one of them in the
/// single `LOCK TABLE` statement its timeout error echoes — this many make that
/// error about 2 KiB, twice kube-rs's stderr pipe.
const TABLES: usize = 60;

fn opted_in() -> (tokio::runtime::Runtime, kube::Client, KubeRsExec) {
    // Explicitly opted in, so a missing precondition is a FAILURE, not a skip.
    assert_eq!(
        std::env::var("APPRAFTER_K8S_SMOKE").as_deref(),
        Ok("1"),
        "run with APPRAFTER_K8S_SMOKE=1 (this test creates objects in the cluster)"
    );
    assert!(
        std::env::var_os("KUBECONFIG").is_some(),
        "KUBECONFIG must name the kind cluster's kubeconfig explicitly"
    );
    let kc = kube::config::Kubeconfig::read().expect("read the kubeconfig named by KUBECONFIG");
    let ctx = kc.current_context.clone().unwrap_or_default();
    assert!(
        ctx.starts_with("kind-"),
        "refusing to run against context {ctx:?}: this test only targets kind clusters"
    );

    // Built exactly as `main` builds it.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let client = rt
        .block_on(async {
            let config = kube::Config::infer()
                .await
                .map_err(kube::Error::InferConfig)?;
            apprafter_backup::tls::kube_client(config)
        })
        .expect("build the kube client");
    let k = KubeRsExec::new(client.clone(), rt.handle().clone());
    (rt, client, k)
}

/// Creates the namespace, and deletes it however the test ends — a failed
/// assertion included, which is when a leftover server would hurt the rerun.
struct TestNamespace<'a> {
    name: &'static str,
    rt: &'a tokio::runtime::Runtime,
    client: kube::Client,
}

impl<'a> TestNamespace<'a> {
    fn create(name: &'static str, rt: &'a tokio::runtime::Runtime, client: &kube::Client) -> Self {
        rt.block_on(async {
            let api: Api<Namespace> = Api::all(client.clone());
            // A previous run's namespace may still be Terminating, and a
            // Terminating namespace takes the apply but rejects the pods.
            let deadline = Instant::now() + Duration::from_secs(180);
            while let Some(ns) = api.get_opt(name).await.expect("get the test namespace") {
                if ns.status.and_then(|s| s.phase).as_deref() != Some("Terminating") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "namespace {name} is stuck Terminating"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            api.patch(
                name,
                &PatchParams::apply(MANAGER).force(),
                &Patch::Apply(json!({"apiVersion": "v1", "kind": "Namespace",
                                     "metadata": {"name": name}})),
            )
            .await
            .unwrap_or_else(|e| panic!("apply namespace {name}: {e}"));
        });
        Self {
            name,
            rt,
            client: client.clone(),
        }
    }
}

impl Drop for TestNamespace<'_> {
    fn drop(&mut self) {
        let api: Api<Namespace> = Api::all(self.client.clone());
        let _ = self
            .rt
            .block_on(api.delete(self.name, &DeleteParams::default()));
    }
}

/// Run `psql -tAc <sql>` in the server pod and return its trimmed stdout.
fn psql(k: &KubeRsExec, ns: &str, sql: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("psql.out");
    k.exec_stream_to_file(
        "pg",
        ns,
        &[
            "psql",
            "-U",
            "postgres",
            "-v",
            "ON_ERROR_STOP=1",
            "-tAc",
            sql,
        ],
        &out,
        None,
    )
    .unwrap_or_else(|e| panic!("psql {sql:?}: {e}"));
    std::fs::read_to_string(&out)
        .expect("read psql output")
        .trim()
        .to_string()
}

/// How many sessions hold `ACCESS EXCLUSIVE` on table `t1`.
fn exclusive_locks_on_t1(k: &KubeRsExec, ns: &str) -> String {
    psql(
        k,
        ns,
        "SELECT count(*) FROM pg_locks l JOIN pg_class c ON c.oid = l.relation \
         WHERE c.relname = 't1' AND l.mode = 'AccessExclusiveLock' AND l.granted",
    )
}

fn wait_until(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A PostgreSQL 18 server pod behind a Service in `ns`, and the decomposed
/// connection Secret `db-conn` the provisioner would write for it.
fn pg_server(k: &KubeRsExec, rt: &tokio::runtime::Runtime, client: &kube::Client, ns: &str) {
    // --- a PostgreSQL 18 server behind a Service ------------------------------
    k.apply_and_wait_pod_ready(&json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "pg", "namespace": ns, "labels": {"app": "pg"}},
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "postgres", "image": PG_IMAGE, "imagePullPolicy": "IfNotPresent",
                "env": [{"name": "POSTGRES_PASSWORD", "value": "pw"}],
                // TCP, not the socket: the image's init phase serves only the
                // socket, so this turns Ready once the real server is up.
                "readinessProbe": {
                    "exec": {"command": ["pg_isready", "-U", "postgres", "-h", "127.0.0.1"]},
                    "periodSeconds": 1
                }
            }]
        }
    }))
    .expect("the postgres pod reaches Ready");
    rt.block_on(async {
        Api::<Service>::namespaced(client.clone(), ns)
            .patch(
                "pg",
                &PatchParams::apply(MANAGER).force(),
                &Patch::Apply(json!({
                    "apiVersion": "v1", "kind": "Service",
                    "metadata": {"name": "pg", "namespace": ns},
                    "spec": {"selector": {"app": "pg"}, "ports": [{"port": 5432}]}
                })),
            )
            .await
            .expect("apply the Service");
        // The decomposed connection Secret the provisioner writes.
        Api::<Secret>::namespaced(client.clone(), ns)
            .patch(
                "db-conn",
                &PatchParams::apply(MANAGER).force(),
                &Patch::Apply(json!({
                    "apiVersion": "v1", "kind": "Secret",
                    "metadata": {"name": "db-conn", "namespace": ns},
                    "stringData": {"user": "postgres", "pass": "pw",
                                   "host": format!("pg.{ns}.svc"), "port": "5432",
                                   "db": "postgres"}
                })),
            )
            .await
            .expect("apply the connection Secret");
    });
}

/// Run the runner's real extraction of one pg claim on a thread, and give up
/// on it after `watchdog`: a regression then fails the test instead of hanging
/// it. `Err(elapsed)` is the watchdog firing.
fn extract_with_watchdog(
    k: &'static KubeRsExec,
    item: ExtractItem,
    out_dir: &Path,
    watchdog: Duration,
) -> Result<(Duration, cli_core::Result<()>), Duration> {
    let (tx, rx) = mpsc::channel();
    let out_dir = out_dir.to_path_buf();
    let started = Instant::now();
    std::thread::spawn(move || {
        let result = run_extraction(k, std::slice::from_ref(&item), &out_dir, PG_IMAGE);
        let _ = tx.send((started.elapsed(), result));
    });
    rx.recv_timeout(watchdog).map_err(|_| started.elapsed())
}

#[test]
#[ignore = "real cluster, ~6 min — set APPRAFTER_K8S_SMOKE=1 + KUBECONFIG (a kind cluster) to run"]
fn a_held_table_lock_fails_the_dump_within_the_bound_and_a_released_one_dumps() {
    const NS: &str = "apprafter-exec-hang-pg";
    assert_eq!(
        PG_DUMP_LOCK_WAIT_TIMEOUT,
        format!("{}s", LOCK_WAIT.as_secs()),
        "this test's timing is written against the shipped bound"
    );
    let (rt, client, k) = opted_in();
    // `run_extraction` runs on a watchdog thread that may outlive a failed
    // assertion; leaking one exec handle per test process is the price.
    let k: &'static KubeRsExec = Box::leak(Box::new(k));
    let _ns = TestNamespace::create(NS, &rt, &client);

    pg_server(k, &rt, &client, NS);
    psql(
        k,
        NS,
        &format!(
            "CREATE TABLE t1 AS SELECT g AS id FROM generate_series(1, 1000) g; \
             DO $$ BEGIN FOR i IN 1..{TABLES} LOOP \
               EXECUTE format('CREATE TABLE app_orders_line_items_%s (id int)', i); \
             END LOOP; END $$;"
        ),
    );

    // --- another session holds ACCESS EXCLUSIVE on t1 in an open transaction --
    k.apply_and_wait_pod_ready(&json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "locker", "namespace": NS},
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "psql", "image": PG_IMAGE, "imagePullPolicy": "IfNotPresent",
                "env": [{"name": "PGPASSWORD", "value": "pw"}],
                "command": ["sh", "-c", &format!(
                    "{WAIT_FOR_SERVICE} exec psql -h pg -U postgres -c \
                     'BEGIN; LOCK TABLE t1 IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(3600);'"
                )]
            }]
        }
    }))
    .expect("the locker pod starts");
    wait_until("the locker's lock on t1", Duration::from_secs(60), || {
        exclusive_locks_on_t1(k, NS) == "1"
    });

    let item = ExtractItem {
        namespace: NS.to_string(),
        claim_name: "db".to_string(),
        kind: DataKind::Pg,
        source: "db-conn".to_string(),
        connection: None,
    };
    let dir = tempfile::tempdir().expect("tempdir");

    // --- while the lock is held: fails after the bound, naming the lock -------
    let blocked = extract_with_watchdog(k, item.clone(), dir.path(), LOCK_WAIT + SETUP_SLACK);
    let (elapsed, result) = blocked.unwrap_or_else(|waited| {
        panic!(
            "the dump behind a held lock was still running after {waited:?}; \
             its lock wait is bounded at {LOCK_WAIT:?}"
        )
    });
    let err = result.expect_err("a dump that never got its locks must fail the run");
    let msg = err.to_string();
    eprintln!("blocked dump failed after {elapsed:?}:\n{msg}");
    assert!(
        elapsed >= LOCK_WAIT,
        "failed after {elapsed:?}, before the lock wait could have run out: {msg}"
    );
    assert!(
        msg.starts_with(&format!("pg dump of {NS}/db gave up")),
        "the lock is named first: {msg}"
    );
    assert!(
        msg.contains("canceling statement due to statement timeout"),
        "{msg}"
    );
    // The last table of the ~2 KiB LOCK TABLE statement: the whole of
    // pg_dump's stderr came through kube-rs's 1 KiB pipe.
    assert!(
        msg.contains(&format!(
            "public.app_orders_line_items_{TABLES} IN ACCESS SHARE MODE"
        )),
        "pg_dump's stderr was cut short: {msg}"
    );

    // --- lock released: the same extraction dumps ------------------------------
    k.delete_pod_best_effort("locker", NS);
    wait_until("the lock on t1 to go", Duration::from_secs(60), || {
        exclusive_locks_on_t1(k, NS) == "0"
    });
    let (elapsed, result) = extract_with_watchdog(k, item, dir.path(), SETUP_SLACK)
        .unwrap_or_else(|waited| panic!("the unblocked dump was still running after {waited:?}"));
    result.unwrap_or_else(|e| panic!("the unblocked dump failed after {elapsed:?}: {e}"));
    let dump = std::fs::read(dir.path().join("pg").join(NS).join("db.dump")).expect("the dump");
    assert!(dump.starts_with(b"PGDMP"), "not a custom-format dump");
    let has = |needle: &[u8]| dump.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"t1"), "the locked table is in the dump");
    assert!(has(format!("app_orders_line_items_{TABLES}").as_bytes()));
}

#[test]
#[ignore = "real cluster — set APPRAFTER_K8S_SMOKE=1 + KUBECONFIG (a kind cluster) to run"]
fn a_command_writing_more_stderr_than_the_exec_pipe_holds_still_returns() {
    const NS: &str = "apprafter-exec-hang-stderr";
    let (rt, client, k) = opted_in();
    let k: &'static KubeRsExec = Box::leak(Box::new(k));
    let _ns = TestNamespace::create(NS, &rt, &client);

    k.apply_and_wait_pod_ready(&json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "noisy", "namespace": NS},
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "sh", "image": PG_IMAGE, "imagePullPolicy": "IfNotPresent",
                "command": ["sleep", "3600"]
            }]
        }
    }))
    .expect("the pod reaches Ready");

    // 64 KiB of stderr between a first and a last line, a little stdout, and a
    // failure: the shape of a tar that warned about every file and then died.
    let script = "echo 'FIRST: the command started' >&2; \
                  head -c 65536 /dev/zero | tr '\\0' w >&2; \
                  echo >&2; echo 'LAST: exiting 3' >&2; \
                  printf 'partial'; exit 3";
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("out");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(k.exec_stream_to_file("noisy", NS, &["sh", "-c", script], &out, None));
    });
    let result = rx
        .recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| {
            panic!("the exec is still running 60 s in: its stderr is not being read")
        });

    let msg = result.expect_err("exit 3 must fail the step").to_string();
    assert!(msg.contains("NonZeroExitCode"), "{msg}");
    assert!(msg.contains("FIRST: the command started"), "{msg}");
    assert!(msg.contains("LAST: exiting 3"), "{msg}");
    assert!(msg.contains("bytes of stderr not kept"), "{msg}");
    assert!(
        msg.len() < 8 * 1024,
        "the error is bounded: {} bytes",
        msg.len()
    );
}

/// How many sessions wait on a lock inside `pg_get_viewdef` — the catalog read
/// `pg_dump` makes for a view or materialized view after its table locks.
fn dumps_waiting_on_a_view_definition(k: &KubeRsExec, ns: &str) -> String {
    psql(
        k,
        ns,
        "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' \
         AND query LIKE '%pg_get_viewdef%'",
    )
}

#[test]
#[ignore = "real cluster, ~12 min — set APPRAFTER_K8S_SMOKE=1 + KUBECONFIG (a kind cluster) to run"]
fn a_held_materialized_view_lock_fails_the_dump_at_its_first_output_bound_and_a_released_one_dumps()
{
    const NS: &str = "apprafter-exec-hang-mv";
    let (rt, client, k) = opted_in();
    let k: &'static KubeRsExec = Box::leak(Box::new(k));
    let _ns = TestNamespace::create(NS, &rt, &client);
    pg_server(k, &rt, &client, NS);
    psql(
        k,
        NS,
        "CREATE TABLE t1 AS SELECT g AS id FROM generate_series(1, 1000) g; \
         CREATE MATERIALIZED VIEW mv1 AS SELECT count(*) AS n FROM t1;",
    );

    // --- a REFRESH MATERIALIZED VIEW held in an open transaction -------------
    // ACCESS EXCLUSIVE on the materialized view, and nothing on t1 that
    // conflicts with the dump's ACCESS SHARE: its LOCK TABLE goes through, and
    // --lock-wait-timeout never gets the chance to fire.
    k.apply_and_wait_pod_ready(&json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "refresher", "namespace": NS},
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "psql", "image": PG_IMAGE, "imagePullPolicy": "IfNotPresent",
                "env": [{"name": "PGPASSWORD", "value": "pw"}],
                "command": ["sh", "-c", &format!(
                    "{WAIT_FOR_SERVICE} exec psql -h pg -U postgres -c \
                     'BEGIN; REFRESH MATERIALIZED VIEW mv1; SELECT pg_sleep(3600);'"
                )]
            }]
        }
    }))
    .expect("the refresher pod starts");
    let held_on_mv1 = || {
        psql(
            k,
            NS,
            "SELECT count(*) FROM pg_locks l JOIN pg_class c ON c.oid = l.relation \
             WHERE c.relname = 'mv1' AND l.mode = 'AccessExclusiveLock' AND l.granted",
        )
    };
    wait_until(
        "the refresher's lock on mv1",
        Duration::from_secs(60),
        || held_on_mv1() == "1",
    );

    let item = ExtractItem {
        namespace: NS.to_string(),
        claim_name: "db".to_string(),
        kind: DataKind::Pg,
        source: "db-conn".to_string(),
        connection: None,
    };
    let dir = tempfile::tempdir().expect("tempdir");

    // --- while the lock is held: silent, then abandoned at the bound ---------
    let (tx, rx) = mpsc::channel();
    {
        let item = item.clone();
        let out_dir = dir.path().to_path_buf();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = run_extraction(k, std::slice::from_ref(&item), &out_dir, PG_IMAGE);
            let _ = tx.send((started.elapsed(), result));
        });
    }
    // The dump is waiting on THIS lock, in its catalog read, not on anything
    // else: the explanation below is only true if this holds.
    wait_until(
        "pg_dump to wait on mv1 in pg_get_viewdef",
        SETUP_SLACK,
        || dumps_waiting_on_a_view_definition(k, NS) == "1",
    );
    let (elapsed, result) = rx
        .recv_timeout(PG_DUMP_FIRST_OUTPUT_WITHIN + SETUP_SLACK)
        .unwrap_or_else(|_| {
            panic!(
                "the dump behind a held materialized-view lock was still running after \
                 {:?}; its first output is bounded at {PG_DUMP_FIRST_OUTPUT_WITHIN:?}",
                PG_DUMP_FIRST_OUTPUT_WITHIN + SETUP_SLACK
            )
        });
    let msg = result
        .expect_err("a dump that never wrote a byte must fail the run")
        .to_string();
    eprintln!("blocked dump failed after {elapsed:?}:\n{msg}");
    assert!(
        elapsed >= PG_DUMP_FIRST_OUTPUT_WITHIN,
        "failed after {elapsed:?}, before the first-output bound: {msg}"
    );
    assert!(
        msg.starts_with(&format!(
            "pg dump of {NS}/db gave up: pg_dump wrote nothing"
        )),
        "the wait is explained first: {msg}"
    );
    assert!(msg.contains("REFRESH MATERIALIZED VIEW"), "{msg}");
    assert!(msg.contains(backup_core::kube::NO_OUTPUT_MARKER), "{msg}");
    // Not the table-lock path: pg_dump got its table locks and never timed out.
    assert!(
        !msg.contains("canceling statement due to statement timeout"),
        "{msg}"
    );

    // --- lock released: the same extraction dumps ------------------------------
    k.delete_pod_best_effort("refresher", NS);
    wait_until("the lock on mv1 to go", Duration::from_secs(60), || {
        held_on_mv1() == "0"
    });
    // The abandoned dump's helper pod is on its way out (the guard deleted
    // it); the next extraction reuses the name, so let it go first.
    let pods: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client.clone(), NS);
    wait_until(
        "the abandoned helper pod to go",
        Duration::from_secs(120),
        || {
            rt.block_on(pods.get_opt("bk-pg-db"))
                .expect("get the helper pod")
                .is_none()
        },
    );
    let (elapsed, result) = extract_with_watchdog(k, item, dir.path(), SETUP_SLACK)
        .unwrap_or_else(|waited| panic!("the unblocked dump was still running after {waited:?}"));
    result.unwrap_or_else(|e| panic!("the unblocked dump failed after {elapsed:?}: {e}"));
    let dump = std::fs::read(dir.path().join("pg").join(NS).join("db.dump")).expect("the dump");
    assert!(dump.starts_with(b"PGDMP"), "not a custom-format dump");
    assert!(
        dump.windows(3).any(|w| w == b"mv1"),
        "the materialized view is in the dump"
    );
}
