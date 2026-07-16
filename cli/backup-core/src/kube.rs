// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `KubeExec` — abstract interface over the kubectl operations the backup
//! engine needs. Implemented by `KubectlExec` (CLI subprocess path) and,
//! in a later phase, by a kube-rs in-cluster runner.

use cli_core::Result;
use serde_json::Value;
use std::path::Path;

/// Kubectl-level operations the backup/restore engine needs. The CLI provides
/// `platform_cli::commands::backup::KubectlExec` (subprocess); the future
/// in-cluster runner provides a kube-rs implementation.
///
/// # Signature notes
/// * `exec_stream_to_file` / `exec_stream_from_file` take `argv: &[&str]`
///   because every call site in `extract.rs` / `restore.rs` builds a
///   `Vec<&str>` and passes a slice. Using `&[String]` would force the
///   callers to allocate a new `Vec<String>` for no reason.
/// * `get_json` is the general "run kubectl and return parsed JSON" method
///   used by the orchestration layer. `Ok(None)` means 404 / not found.
///   Callers tolerate missing CRDs by checking the `Err` message text (as
///   `backup.rs::list_items` does today).
/// * `get_secret_key` runs the jsonpath probe that returns a single
///   base64-encoded key value (not JSON) — it is separate from `get_json`
///   because the raw bytes need a different decode path.
pub trait KubeExec {
    /// `kubectl apply -f -` (stdin JSON) + `kubectl wait --for=condition=Ready`.
    fn apply_and_wait_pod_ready(&self, spec: &Value) -> Result<()>;

    /// `kubectl exec <pod> -n <ns> -- <argv...>` → stdout streamed to `out`.
    fn exec_stream_to_file(&self, pod: &str, ns: &str, argv: &[&str], out: &Path) -> Result<()>;

    /// `kubectl exec -i <pod> -n <ns> -- <argv...>` ← stdin fed from `input`.
    fn exec_stream_from_file(&self, pod: &str, ns: &str, argv: &[&str], input: &Path)
        -> Result<()>;

    /// `kubectl delete pod <name> -n <ns> --ignore-not-found --wait=false`
    /// (best-effort; errors are silently swallowed).
    fn delete_pod_best_effort(&self, name: &str, ns: &str);

    /// `kubectl get secret <secret> -n <ns> -o jsonpath={.data.<key>}`,
    /// then base64-decode and return as UTF-8.
    fn get_secret_key(&self, secret: &str, ns: &str, key: &str) -> Result<String>;

    /// Run `kubectl <args...> -o json` and return the parsed `Value`.
    /// Returns `Ok(None)` on 404 / not-found; propagates other errors.
    /// The caller is responsible for appending `-o json` to `args` when
    /// needed — or for passing args that already include it.
    fn get_json(&self, args: &[&str]) -> Result<Option<Value>>;
}
