// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Ephemeral helper-pod spec builders (pure) + kubectl-exec stream helpers
//! (impure) for 2.6d backup/restore data extraction and load.
//!
//! # Pure pod-spec builders
//!
//! `pg_dump_pod_spec` / `volume_pod_spec` return serde_json::Value Pod specs
//! that are applied via `apply_and_wait_pod_ready` and then exec'd into.
//! Both use `["sleep", "3600"]` as the container command — a keep-alive so
//! the CLI can exec before the tool needs to run.  `restartPolicy: Never`
//! ensures a single attempt; the caller tears down with
//! `delete_pod_best_effort` after the stream completes.
//!
//! # SIGPIPE / drainer-thread pattern (impure)
//!
//! `kubectl` is a Go binary.  Go's default SIGPIPE handler terminates the
//! process on the next write to a closed stdout pipe.  If we drop the read
//! end too early (e.g. the main thread finishes its copy and returns) the
//! in-pod `pg_dump` / `tar` receives SIGPIPE and may corrupt its output
//! mid-stream.
//!
//! Mirror of the pattern in `platform-cli::commands::port_forward` — which
//! cannot be imported here because `cli-providers` is a lib crate and
//! `platform-cli` is the bin crate (lib→bin dependency is forbidden).  We
//! reimplement it std-only: a spawned thread drains stderr via
//! `BufReader::new(reader).lines()` capturing into
//! `Arc<Mutex<Vec<String>>>`, the main thread does the real I/O (stdout
//! drain for extraction; stdin feed for load), then `child.wait()` surfaces
//! the exit code.
//!
//! # BrokenPipe (write-side / LoadData L2)
//!
//! Rust ignores `SIGPIPE` process-wide (it has been SIG_IGN since Rust 1.x
//! on Linux).  When the in-pod restore tool exits early the write to child
//! stdin surfaces as `io::Error` with kind `BrokenPipe` instead of killing
//! the CLI process.  We MUST NOT propagate that error directly; instead we
//! stop writing, drop stdin (sends EOF), and call `child.wait()` to get the
//! real exit code + captured stderr (which will tell us why the tool died).

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cli_core::{CliError, Result};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum stderr lines to retain for error reporting.
const STDERR_CAPTURE_LIMIT: usize = 20;

/// Grace period after `child.wait()` to let the stderr-drainer thread flush
/// its last line into the buffer before we read it.
const STDERR_FLUSH_GRACE_MS: u64 = 100;

// ---------------------------------------------------------------------------
// Pure pod-spec builders
// ---------------------------------------------------------------------------

/// Build a Pod spec for pg_dump extraction.
///
/// No PVC mount — the container runs `pg_dump` over a TCP connection to the
/// CNPG cluster Service.  The keep-alive command (`sleep 3600`) lets the CLI
/// exec in and run the tool after the pod reaches Running.
pub fn pg_dump_pod_spec(name: &str, ns: &str, image: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "apprafter.io/backup-helper": "true" }
        },
        "spec": {
            "restartPolicy": "Never",
            "containers": [{
                "name": "dump",
                "image": image,
                "command": ["sleep", "3600"]
            }]
        }
    })
}

/// Build a Pod spec that mounts a PersistentVolumeClaim at `/data`.
///
/// `read_only` = `true` for backup extraction (read the volume tree out into
/// a tar stream); `false` for `LoadData` restore (write the tree into a
/// freshly-provisioned PVC).  The distinction matters for the
/// `persistentVolumeClaim.readOnly` field and the matching `volumeMounts`
/// entry — Kubernetes enforces the readOnly flag on the mount, and a
/// RWO PVC mounted read-write on restore allows `tar x` to write files (L1).
pub fn volume_pod_spec(name: &str, ns: &str, image: &str, pvc: &str, read_only: bool) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "apprafter.io/backup-helper": "true" }
        },
        "spec": {
            "restartPolicy": "Never",
            "volumes": [{
                "name": "data",
                "persistentVolumeClaim": {
                    "claimName": pvc,
                    "readOnly": read_only
                }
            }],
            "containers": [{
                "name": "dump",
                "image": image,
                "command": ["sleep", "3600"],
                "volumeMounts": [{
                    "name": "data",
                    "mountPath": "/data",
                    "readOnly": read_only
                }]
            }]
        }
    })
}

// ---------------------------------------------------------------------------
// Drainer-thread helper (std-only reimplementation of the port_forward.rs
// pattern — lib crate cannot depend on the bin crate platform-cli)
// ---------------------------------------------------------------------------

/// Spawn a thread that drains `reader` to EOF, retaining the last
/// `STDERR_CAPTURE_LIMIT` lines in a shared buffer for error reporting.
fn spawn_capturing_drainer<R: Read + Send + 'static>(reader: R) -> Arc<Mutex<Vec<String>>> {
    let buf: Arc<Mutex<Vec<String>>> =
        Arc::new(Mutex::new(Vec::with_capacity(STDERR_CAPTURE_LIMIT)));
    let buf_clone = Arc::clone(&buf);
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let mut guard = buf_clone.lock().unwrap();
            if guard.len() >= STDERR_CAPTURE_LIMIT {
                guard.remove(0);
            }
            guard.push(line);
        }
    });
    buf
}

/// Format the "child exited non-zero" error, attaching captured stderr lines.
fn format_exec_error(
    context: &str,
    status: std::process::ExitStatus,
    stderr_buf: &Arc<Mutex<Vec<String>>>,
) -> CliError {
    // Give the drainer thread a brief window to flush its last line.
    thread::sleep(Duration::from_millis(STDERR_FLUSH_GRACE_MS));
    let captured = stderr_buf.lock().unwrap();
    if captured.is_empty() {
        CliError::Other(format!(
            "{context}: kubectl exec exited with {status} and produced no stderr output"
        ))
    } else {
        let text = captured.join("\n");
        CliError::Other(format!(
            "{context}: kubectl exec exited with {status}.\nkubectl stderr:\n  {}",
            text.replace('\n', "\n  ")
        ))
    }
}

// ---------------------------------------------------------------------------
// Impure stream functions (NOT unit-tested — require a live cluster)
// ---------------------------------------------------------------------------

/// Stream data DOWN from a pod to a local file (backup extraction).
///
/// Spawns `kubectl exec <pod> -n <ns> -- <argv...>` with piped stdout and
/// stderr.  The main thread copies stdout → `out_path` to EOF (consuming
/// every byte so the in-pod tool is never SIGPIPE'd); a drainer thread
/// captures stderr.  Non-zero exit → `Err` with the captured stderr text.
///
/// `kubeconfig` must remain alive for the duration of the call (the caller
/// holds a `NamedTempFile` — do NOT let it drop).
pub fn exec_stream_to_file(
    pod: &str,
    ns: &str,
    argv: &[&str],
    out_path: &Path,
    kubeconfig: &Path,
) -> Result<()> {
    let mut cmd = Command::new("kubectl");
    cmd.arg("exec")
        .arg(pod)
        .arg("-n")
        .arg(ns)
        .arg("--")
        .args(argv)
        .env("KUBECONFIG", kubeconfig)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| CliError::Other(format!("spawn kubectl exec (stream-to-file): {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::Other("kubectl exec has no stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CliError::Other("kubectl exec has no stderr".into()))?;

    // Drainer thread: keep stderr consumed so the pipe buffer never fills and
    // blocks the in-pod tool; also capture lines for error reporting.
    let stderr_buf = spawn_capturing_drainer(stderr);

    // Main thread: copy stdout → file to EOF.
    // MUST consume to EOF — dropping the read end early sends SIGPIPE to the
    // Go kubectl binary, which propagates it into the in-pod pg_dump/tar.
    let mut out_file = File::create(out_path)
        .map_err(|e| CliError::Other(format!("create output file {}: {e}", out_path.display())))?;
    let mut reader = BufReader::new(stdout);
    io::copy(&mut reader, &mut out_file)
        .map_err(|e| CliError::Other(format!("copy kubectl exec stdout → file: {e}")))?;

    let status = child
        .wait()
        .map_err(|e| CliError::Other(format!("wait kubectl exec: {e}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(format_exec_error(
            "exec_stream_to_file",
            status,
            &stderr_buf,
        ))
    }
}

/// Stream data UP from a local file into a pod (restore / LoadData L2).
///
/// Spawns `kubectl exec -i <pod> -n <ns> -- <argv...>` with piped stdin and
/// stderr.  The main thread copies `in_path` → child stdin, then drops stdin
/// (sending EOF to the in-pod restore tool); a drainer thread captures
/// stderr.  Non-zero exit → `Err` with the captured stderr text.
///
/// # BrokenPipe on the write side (L2)
///
/// Rust ignores `SIGPIPE` process-wide; when the in-pod tool exits early the
/// write to child stdin surfaces as `io::Error` with `kind() ==
/// BrokenPipe` instead of crashing the CLI.  We treat `BrokenPipe` as "the
/// child exited" — stop writing, drop stdin (EOF), then `child.wait()` and
/// surface ITS stderr as the real cause.  Propagating the `BrokenPipe`
/// directly would be wrong: it looks like an I/O error but the actual failure
/// is inside the pod (e.g. `pg_restore` rejected the dump format).
///
/// `kubeconfig` must remain alive for the duration of the call (the caller
/// holds a `NamedTempFile` — do NOT let it drop).
pub fn exec_stream_from_file(
    pod: &str,
    ns: &str,
    argv: &[&str],
    in_path: &Path,
    kubeconfig: &Path,
) -> Result<()> {
    let mut cmd = Command::new("kubectl");
    cmd.arg("exec")
        .arg("-i")
        .arg(pod)
        .arg("-n")
        .arg(ns)
        .arg("--")
        .args(argv)
        .env("KUBECONFIG", kubeconfig)
        .stdin(Stdio::piped())
        .stdout(Stdio::null()) // restore tools write to DB, not stdout
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| CliError::Other(format!("spawn kubectl exec (stream-from-file): {e}")))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CliError::Other("kubectl exec has no stdin".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CliError::Other("kubectl exec has no stderr".into()))?;

    // Drainer thread: keep stderr consumed and captured.
    let stderr_buf = spawn_capturing_drainer(stderr);

    // Main thread: copy file → stdin.
    //
    // BrokenPipe handling (L2): if the in-pod tool exits early, the write
    // surfaces as io::Error { kind: BrokenPipe }.  Do NOT propagate it.
    // Drop stdin (sends EOF), then child.wait() for the real exit code +
    // stderr (the actual cause — e.g. pg_restore rejected the dump format).
    let mut in_file = File::open(in_path)
        .map_err(|e| CliError::Other(format!("open input file {}: {e}", in_path.display())))?;
    match io::copy(&mut in_file, &mut stdin) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
            // The in-pod tool exited early.  Drop stdin (EOF) and let
            // child.wait() surface the real error from its stderr.
        }
        Err(e) => {
            return Err(CliError::Other(format!(
                "copy file → kubectl exec stdin: {e}"
            )));
        }
    }
    // Explicitly drop stdin to send EOF to the in-pod tool (if it is still
    // running after a full read) before we wait.
    drop(stdin);

    let status = child
        .wait()
        .map_err(|e| CliError::Other(format!("wait kubectl exec: {e}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(format_exec_error(
            "exec_stream_from_file",
            status,
            &stderr_buf,
        ))
    }
}

/// Apply a Pod spec JSON and block until the pod reaches `Ready`.
///
/// The spec is passed via `kubectl apply -f -` on stdin (JSON).  Then
/// `kubectl wait --for=condition=Ready pod/<name>` with a 5-minute timeout.
///
/// `kubeconfig` must remain alive for the duration of the call.
pub fn apply_and_wait_pod_ready(spec: &Value, kubeconfig: &Path) -> Result<()> {
    let name = spec["metadata"]["name"]
        .as_str()
        .ok_or_else(|| CliError::Other("pod spec missing metadata.name".into()))?;
    let ns = spec["metadata"]["namespace"]
        .as_str()
        .ok_or_else(|| CliError::Other("pod spec missing metadata.namespace".into()))?;

    // Apply the spec via stdin.
    let json_bytes = serde_json::to_vec(spec)
        .map_err(|e| CliError::Other(format!("serialize pod spec: {e}")))?;

    let mut apply_child = Command::new("kubectl")
        .args(["apply", "-f", "-", "-n", ns])
        .env("KUBECONFIG", kubeconfig)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CliError::Other(format!("spawn kubectl apply: {e}")))?;

    {
        let mut stdin = apply_child
            .stdin
            .take()
            .ok_or_else(|| CliError::Other("kubectl apply has no stdin".into()))?;
        stdin
            .write_all(&json_bytes)
            .map_err(|e| CliError::Other(format!("write pod spec to kubectl apply: {e}")))?;
        // stdin dropped here → EOF
    }

    let apply_stderr = apply_child
        .stderr
        .take()
        .ok_or_else(|| CliError::Other("kubectl apply has no stderr".into()))?;
    let apply_stderr_buf = spawn_capturing_drainer(apply_stderr);
    let apply_status = apply_child
        .wait()
        .map_err(|e| CliError::Other(format!("wait kubectl apply: {e}")))?;
    if !apply_status.success() {
        return Err(format_exec_error(
            "apply_and_wait_pod_ready(apply)",
            apply_status,
            &apply_stderr_buf,
        ));
    }

    // Wait for Ready with a generous timeout (image pull on slow nodes).
    let wait_status = Command::new("kubectl")
        .args([
            "wait",
            "--for=condition=Ready",
            &format!("pod/{name}"),
            "-n",
            ns,
            "--timeout=300s",
        ])
        .env("KUBECONFIG", kubeconfig)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| CliError::Other(format!("spawn kubectl wait: {e}")))?;

    if wait_status.success() {
        Ok(())
    } else {
        Err(CliError::Other(format!(
            "pod {name} in {ns} did not reach Ready within 300s (kubectl wait exited {wait_status})"
        )))
    }
}

/// Delete the helper pod, ignoring errors (best-effort cleanup guard).
///
/// Called unconditionally in a `finally`-style pattern — after the data
/// stream completes OR on error paths.  `--ignore-not-found` makes this
/// idempotent; failure is silently swallowed so callers can propagate the
/// original error.
pub fn delete_pod_best_effort(name: &str, ns: &str, kubeconfig: &Path) {
    let _ = Command::new("kubectl")
        .args([
            "delete",
            "pod",
            name,
            "-n",
            ns,
            "--ignore-not-found",
            "--wait=false",
        ])
        .env("KUBECONFIG", kubeconfig)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// ---------------------------------------------------------------------------
// Tests (pure pod-spec builders only — impure fns require a live cluster)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_dump_pod_uses_pg_image_and_no_pvc_mount() {
        let p = pg_dump_pod_spec("bk-pg-alpha", "demo", "postgres:16-alpine");
        assert_eq!(p["spec"]["containers"][0]["image"], "postgres:16-alpine");
        assert_eq!(p["metadata"]["namespace"], "demo");
        assert!(p["spec"]["containers"][0]["command"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "sleep"));
        assert!(p["spec"].get("volumes").is_none());
        assert_eq!(p["spec"]["restartPolicy"], "Never");
    }

    #[test]
    fn volume_pod_mounts_pvc_read_only_for_backup() {
        let p = volume_pod_spec(
            "bk-vol-data",
            "demo",
            "busybox:1.36",
            "sv-demo-shared",
            true,
        );
        let vol = &p["spec"]["volumes"][0];
        assert_eq!(vol["persistentVolumeClaim"]["claimName"], "sv-demo-shared");
        assert_eq!(vol["persistentVolumeClaim"]["readOnly"], true);
        assert_eq!(
            p["spec"]["containers"][0]["volumeMounts"][0]["readOnly"],
            true
        );
    }

    #[test]
    fn volume_pod_rw_for_restore_load() {
        // L1: LoadData must WRITE the tree into the fresh PVC → read_only=false
        let p = volume_pod_spec("ld-vol-data", "demo", "busybox:1.36", "claim-x", false);
        assert_eq!(
            p["spec"]["volumes"][0]["persistentVolumeClaim"]["readOnly"],
            false
        );
        assert_eq!(
            p["spec"]["containers"][0]["volumeMounts"][0]["readOnly"],
            false
        );
    }
}
