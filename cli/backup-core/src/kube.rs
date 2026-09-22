// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `KubeExec` — abstract interface over the kubectl operations the backup
//! engine needs. Implemented by `KubectlExec` (CLI subprocess path) and,
//! in a later phase, by a kube-rs in-cluster runner.

use cli_core::{CliError, Result};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

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
    ///
    /// `first_output_within`: when `Some(bound)`, the command must write its
    /// first byte to stdout within `bound` of the exec starting. If it has not,
    /// the exec is abandoned and the call fails with [`no_output_error`]. Only
    /// the first byte is timed: a command that has started writing may take as
    /// long as it needs. `None` waits for as long as the command runs.
    fn exec_stream_to_file(
        &self,
        pod: &str,
        ns: &str,
        argv: &[&str],
        out: &Path,
        first_output_within: Option<Duration>,
    ) -> Result<()>;

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

/// The words every [`KubeExec`] implementation puts in the error it returns
/// when a command wrote nothing to stdout within its `first_output_within`
/// bound. [`is_no_output_error`] matches on them, so the caller that set the
/// bound can explain it (`extract::explain_pg_dump_error`) without knowing
/// which implementation ran.
pub const NO_OUTPUT_MARKER: &str = "wrote nothing to stdout within";

/// The error for a command that wrote nothing to stdout within `bound`. Both
/// implementations build it here, so its wording cannot drift from
/// [`is_no_output_error`].
pub fn no_output_error(
    context: &str,
    argv: &[&str],
    ns: &str,
    pod: &str,
    bound: Duration,
) -> CliError {
    CliError::Other(format!(
        "{context}: {argv:?} in {ns}/{pod} {NO_OUTPUT_MARKER} {}s of starting, so the exec was \
         abandoned",
        bound.as_secs()
    ))
}

/// Whether `err` is the [`no_output_error`] of an exec.
pub fn is_no_output_error(err: &CliError) -> bool {
    err.to_string().contains(NO_OUTPUT_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_no_output_error_names_the_command_and_the_bound_and_is_recognised() {
        let e = no_output_error(
            "exec_stream_to_file",
            &["pg_dump", "-Fc"],
            "demo",
            "bk-pg-db",
            Duration::from_secs(600),
        );
        let msg = e.to_string();
        assert!(msg.contains("[\"pg_dump\", \"-Fc\"]"), "{msg}");
        assert!(msg.contains("demo/bk-pg-db"), "{msg}");
        assert!(msg.contains("600s"), "{msg}");
        assert!(is_no_output_error(&e));
        assert!(!is_no_output_error(&CliError::Other(
            "exec_stream_to_file: exec failed (status=Failure)".into()
        )));
    }
}
