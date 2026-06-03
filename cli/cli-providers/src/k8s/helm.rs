// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Wrapper around the `helm` binary. Real impl shells out;
//! tests inject fakes via the `HelmRunner` trait.

use std::path::{Path, PathBuf};
use std::process::Command;

use cli_core::{CliError, Result};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct HelmUpgradeArgs {
    pub release: String,
    pub chart: String,
    /// Chart version override. `None` means "use whatever is in
    /// the chart's `Chart.yaml`" — necessary when the chart is
    /// referenced by local-path (e.g. an embedded helm chart
    /// extracted to a tempdir).
    pub version: Option<String>,
    pub namespace: String,
    pub values_path: PathBuf,
    pub kubeconfig_path: PathBuf,
    /// Extra `--set key=value` overrides applied on top of
    /// `values_path`. Used by `cluster-bootstrap` to flip
    /// `ipv6.enabled=false` on Cilium for single-stack (IPv4-only)
    /// clusters. Empty for the common case.
    pub set_values: Vec<String>,
    /// Loader fingerprint injected as `--set apprafterLoaderHash=<fp>`
    /// so helm records it in the release config. Read back on a
    /// re-run by [`HelmRunner::release_is_current`] to decide whether
    /// the `upgrade_install` can be skipped (no needless revision
    /// bump). `None` for the bare args before the fingerprint is
    /// computed; [`loader_fingerprint`] must NOT see this field set
    /// (it would feed the hash into itself). Set by
    /// `cluster-bootstrap`'s `upgrade_unless_current` right before
    /// the real upgrade.
    pub fingerprint: Option<String>,
}

/// Deterministic fingerprint of everything the loader controls for a
/// release: the chart ref, the pinned version, the values-file CONTENT,
/// and the sorted `--set` overrides. Two runs of the same CLI version
/// produce the same fingerprint; a CLI upgrade that changes the chart,
/// version, values, or overrides produces a different one. NOTE: does
/// NOT include the injected `apprafterLoaderHash` itself.
pub fn loader_fingerprint(args: &HelmUpgradeArgs) -> Result<String> {
    let values = std::fs::read(&args.values_path)
        .map_err(|e| CliError::Other(format!("read values {}: {e}", args.values_path.display())))?;
    let mut sorted = args.set_values.clone();
    sorted.sort();
    let mut h = Sha256::new();
    h.update(args.chart.as_bytes());
    h.update([0]);
    h.update(args.version.as_deref().unwrap_or("").as_bytes());
    h.update([0]);
    h.update(&values);
    h.update([0]);
    h.update(sorted.join("\0").as_bytes());
    Ok(format!("{:x}", h.finalize()))
}

pub trait HelmRunner {
    fn repo_add(&self, name: &str, url: &str) -> Result<()>;
    fn upgrade_install(&self, args: &HelmUpgradeArgs) -> Result<()>;
    /// True iff the release is already `deployed` at exactly the desired
    /// loader fingerprint — i.e. a re-run would be a no-op, so the
    /// `upgrade_install` can be skipped. False when the release is absent,
    /// in a non-deployed status (failed/pending → re-upgrade to repair),
    /// or carries a different/absent `apprafterLoaderHash`.
    fn release_is_current(
        &self,
        release: &str,
        namespace: &str,
        kubeconfig: &Path,
        fingerprint: &str,
    ) -> Result<bool>;
}

#[derive(Debug, Default)]
pub struct HelmCli;

impl HelmCli {
    fn build_repo_add_command(name: &str, url: &str) -> Command {
        let mut c = Command::new("helm");
        c.arg("repo")
            .arg("add")
            .arg(name)
            .arg(url)
            .arg("--force-update");
        c
    }

    fn build_upgrade_install_command(args: &HelmUpgradeArgs) -> Command {
        let mut c = Command::new("helm");
        c.arg("upgrade")
            .arg("--install")
            .arg(&args.release)
            .arg(&args.chart);
        if let Some(v) = &args.version {
            c.arg("--version").arg(v);
        }
        c.arg("--namespace")
            .arg(&args.namespace)
            .arg("--create-namespace")
            .arg("--values")
            .arg(&args.values_path)
            .arg("--wait")
            .env("KUBECONFIG", &args.kubeconfig_path);
        for kv in &args.set_values {
            c.arg("--set").arg(kv);
        }
        // Inject the loader fingerprint so helm records it in the
        // release config. Cilium/Argo ignore the unknown value; we
        // read it back on a re-run via `helm get values` to decide
        // whether the upgrade can be skipped. Appended LAST so it
        // never feeds back into `loader_fingerprint` (which hashes
        // only `set_values`, not this synthetic override).
        if let Some(fp) = &args.fingerprint {
            c.arg("--set").arg(format!("apprafterLoaderHash={fp}"));
        }
        c
    }

    /// `helm list -n <ns> --filter "^<release>$" -o json` (+ KUBECONFIG).
    /// The anchored filter matches the single release by exact name.
    fn build_list_command(release: &str, namespace: &str, kubeconfig: &Path) -> Command {
        let mut c = Command::new("helm");
        c.arg("list")
            .arg("--namespace")
            .arg(namespace)
            .arg("--filter")
            .arg(format!("^{release}$"))
            .arg("--output")
            .arg("json")
            .env("KUBECONFIG", kubeconfig);
        c
    }

    /// `helm get values <release> -n <ns> -o json` (+ KUBECONFIG).
    /// Default (user-supplied) values include our injected
    /// `--set apprafterLoaderHash=…`, so it is read back here.
    fn build_get_values_command(release: &str, namespace: &str, kubeconfig: &Path) -> Command {
        let mut c = Command::new("helm");
        c.arg("get")
            .arg("values")
            .arg(release)
            .arg("--namespace")
            .arg(namespace)
            .arg("--output")
            .arg("json")
            .env("KUBECONFIG", kubeconfig);
        c
    }
}

/// Pure parser: the `status` of the single matching release from
/// `helm list -o json` (a JSON array), or `None` when the array is
/// empty / the JSON does not parse. A missing release yields `[]`.
fn parse_release_status(stdout: &str) -> Option<String> {
    let arr: serde_json::Value = serde_json::from_str(stdout).ok()?;
    arr.as_array()?
        .first()?
        .get("status")?
        .as_str()
        .map(|s| s.to_string())
}

/// Pure parser: the `apprafterLoaderHash` string from
/// `helm get values -o json`, or `None` when absent / the body is
/// helm's literal `null` (a release with no user-supplied values) /
/// the JSON does not parse.
fn parse_loader_hash(stdout: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(stdout).ok()?;
    val.get("apprafterLoaderHash")?
        .as_str()
        .map(|s| s.to_string())
}

impl HelmRunner for HelmCli {
    fn repo_add(&self, name: &str, url: &str) -> Result<()> {
        let status = Self::build_repo_add_command(name, url)
            .status()
            .map_err(|e| CliError::Other(format!("spawn helm: {e}")))?;
        if !status.success() {
            return Err(CliError::Other(format!(
                "helm repo add {name} {url} failed (exit {:?})",
                status.code()
            )));
        }
        Ok(())
    }

    fn upgrade_install(&self, args: &HelmUpgradeArgs) -> Result<()> {
        let status = Self::build_upgrade_install_command(args)
            .status()
            .map_err(|e| CliError::Other(format!("spawn helm: {e}")))?;
        if !status.success() {
            return Err(CliError::Other(format!(
                "helm upgrade --install {} failed (exit {:?})",
                args.release,
                status.code()
            )));
        }
        Ok(())
    }

    fn release_is_current(
        &self,
        release: &str,
        namespace: &str,
        kubeconfig: &Path,
        fingerprint: &str,
    ) -> Result<bool> {
        // A non-zero exit for a missing release is NOT an error — it
        // just means "not installed", so the caller should install.
        // We read stdout regardless of exit status; the pure parsers
        // map empty / non-JSON output to `None` (→ not current).
        let list_out = Self::build_list_command(release, namespace, kubeconfig)
            .output()
            .map_err(|e| CliError::Other(format!("spawn helm list: {e}")))?;
        let list_stdout = String::from_utf8_lossy(&list_out.stdout);
        if parse_release_status(&list_stdout).as_deref() != Some("deployed") {
            // Absent, failed, pending, or unparseable → upgrade to
            // install / repair. Never skip.
            return Ok(false);
        }
        let values_out = Self::build_get_values_command(release, namespace, kubeconfig)
            .output()
            .map_err(|e| CliError::Other(format!("spawn helm get values: {e}")))?;
        let values_stdout = String::from_utf8_lossy(&values_out.stdout);
        Ok(parse_loader_hash(&values_stdout).as_deref() == Some(fingerprint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn repo_add_command_includes_force_update() {
        let cmd = HelmCli::build_repo_add_command("cilium", "https://helm.cilium.io/");
        let args = argv(&cmd);
        assert_eq!(
            args,
            vec![
                "repo",
                "add",
                "cilium",
                "https://helm.cilium.io/",
                "--force-update"
            ]
        );
    }

    #[test]
    fn upgrade_install_command_passes_all_required_flags() {
        let cmd = HelmCli::build_upgrade_install_command(&HelmUpgradeArgs {
            release: "cilium".into(),
            chart: "cilium/cilium".into(),
            version: Some("1.16.5".into()),
            namespace: "kube-system".into(),
            values_path: "/tmp/values.yaml".into(),
            kubeconfig_path: "/tmp/kubeconfig".into(),
            set_values: Vec::new(),
            fingerprint: None,
        });
        let args = argv(&cmd);
        for required in [
            "upgrade",
            "--install",
            "cilium",
            "cilium/cilium",
            "--version",
            "1.16.5",
            "--namespace",
            "kube-system",
            "--create-namespace",
            "--values",
            "/tmp/values.yaml",
            "--wait",
        ] {
            assert!(
                args.iter().any(|a| a == required),
                "missing {required}: {args:?}"
            );
        }
        assert!(!args.iter().any(|a| a == "KUBECONFIG"), "{args:?}");
        // No `--set` when set_values is empty.
        assert!(!args.iter().any(|a| a == "--set"), "{args:?}");
    }

    #[test]
    fn upgrade_install_command_appends_set_overrides() {
        let cmd = HelmCli::build_upgrade_install_command(&HelmUpgradeArgs {
            release: "cilium".into(),
            chart: "cilium/cilium".into(),
            version: Some("1.16.5".into()),
            namespace: "kube-system".into(),
            values_path: "/tmp/values.yaml".into(),
            kubeconfig_path: "/tmp/kubeconfig".into(),
            set_values: vec!["ipv6.enabled=false".into()],
            fingerprint: None,
        });
        let args = argv(&cmd);
        let set_idx = args.iter().position(|a| a == "--set");
        assert!(set_idx.is_some(), "expected --set: {args:?}");
        assert_eq!(args[set_idx.unwrap() + 1], "ipv6.enabled=false", "{args:?}");
    }

    #[test]
    fn upgrade_install_command_omits_version_flag_when_none() {
        let cmd = HelmCli::build_upgrade_install_command(&HelmUpgradeArgs {
            release: "apprafter-operator".into(),
            chart: "/tmp/extracted-chart".into(),
            version: None,
            namespace: "apprafter-system".into(),
            values_path: "/tmp/operator-values.yaml".into(),
            kubeconfig_path: "/tmp/kubeconfig".into(),
            set_values: Vec::new(),
            fingerprint: None,
        });
        let args = argv(&cmd);
        assert!(!args.iter().any(|a| a == "--version"), "{args:?}");
        // The rest of the flags must still be present.
        assert!(args.iter().any(|a| a == "--create-namespace"), "{args:?}");
        assert!(args.iter().any(|a| a == "--wait"), "{args:?}");
    }

    fn args_with(values_path: PathBuf, set_values: Vec<String>) -> HelmUpgradeArgs {
        HelmUpgradeArgs {
            release: "cilium".into(),
            chart: "cilium/cilium".into(),
            version: Some("1.16.5".into()),
            namespace: "kube-system".into(),
            values_path,
            kubeconfig_path: "/tmp/kubeconfig".into(),
            set_values,
            fingerprint: None,
        }
    }

    fn write_values(contents: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn upgrade_install_command_appends_loader_hash_when_fingerprint_some() {
        let mut a = args_with("/tmp/values.yaml".into(), Vec::new());
        a.fingerprint = Some("deadbeef".into());
        let cmd = HelmCli::build_upgrade_install_command(&a);
        let args = argv(&cmd);
        let set_idxs: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, v)| *v == "--set")
            .map(|(i, _)| i)
            .collect();
        assert!(
            set_idxs
                .iter()
                .any(|&i| args[i + 1] == "apprafterLoaderHash=deadbeef"),
            "expected --set apprafterLoaderHash=deadbeef: {args:?}"
        );
    }

    #[test]
    fn upgrade_install_command_omits_loader_hash_when_fingerprint_none() {
        // The hash override must NOT appear when fingerprint is None
        // (the bare-args form fed to `loader_fingerprint`).
        let cmd =
            HelmCli::build_upgrade_install_command(&args_with("/tmp/v.yaml".into(), Vec::new()));
        let args = argv(&cmd);
        assert!(
            !args.iter().any(|a| a.starts_with("apprafterLoaderHash=")),
            "{args:?}"
        );
    }

    #[test]
    fn loader_fingerprint_same_inputs_same_hash() {
        let f = write_values("a: 1\nb: 2\n");
        let a1 = args_with(f.path().to_path_buf(), vec!["x=1".into(), "y=2".into()]);
        let a2 = args_with(f.path().to_path_buf(), vec!["x=1".into(), "y=2".into()]);
        assert_eq!(
            loader_fingerprint(&a1).unwrap(),
            loader_fingerprint(&a2).unwrap()
        );
    }

    #[test]
    fn loader_fingerprint_excludes_injected_hash() {
        // The injected `fingerprint` field must NOT change the
        // fingerprint — otherwise it would feed into itself and the
        // recorded hash could never match on a re-run.
        let f = write_values("a: 1\n");
        let bare = args_with(f.path().to_path_buf(), Vec::new());
        let mut injected = bare.clone();
        injected.fingerprint = Some("whatever".into());
        assert_eq!(
            loader_fingerprint(&bare).unwrap(),
            loader_fingerprint(&injected).unwrap()
        );
    }

    #[test]
    fn loader_fingerprint_changes_with_values_content() {
        let f1 = write_values("a: 1\n");
        let f2 = write_values("a: 2\n");
        let a1 = args_with(f1.path().to_path_buf(), Vec::new());
        let a2 = args_with(f2.path().to_path_buf(), Vec::new());
        assert_ne!(
            loader_fingerprint(&a1).unwrap(),
            loader_fingerprint(&a2).unwrap(),
            "a changed values-file content must flip the fingerprint"
        );
    }

    #[test]
    fn loader_fingerprint_set_values_order_independent() {
        let f = write_values("a: 1\n");
        let a1 = args_with(f.path().to_path_buf(), vec!["x=1".into(), "y=2".into()]);
        let a2 = args_with(f.path().to_path_buf(), vec!["y=2".into(), "x=1".into()]);
        assert_eq!(
            loader_fingerprint(&a1).unwrap(),
            loader_fingerprint(&a2).unwrap(),
            "set_values are sorted, so order must not matter"
        );
    }

    #[test]
    fn loader_fingerprint_changes_with_chart_or_version() {
        let f = write_values("a: 1\n");
        let base = args_with(f.path().to_path_buf(), Vec::new());
        let mut diff_chart = base.clone();
        diff_chart.chart = "cilium/other".into();
        let mut diff_version = base.clone();
        diff_version.version = Some("9.9.9".into());
        let bfp = loader_fingerprint(&base).unwrap();
        assert_ne!(bfp, loader_fingerprint(&diff_chart).unwrap());
        assert_ne!(bfp, loader_fingerprint(&diff_version).unwrap());
    }

    #[test]
    fn loader_fingerprint_changes_with_set_overrides() {
        let f = write_values("a: 1\n");
        let none = args_with(f.path().to_path_buf(), Vec::new());
        let some = args_with(f.path().to_path_buf(), vec!["ipv6.enabled=false".into()]);
        assert_ne!(
            loader_fingerprint(&none).unwrap(),
            loader_fingerprint(&some).unwrap()
        );
    }

    #[test]
    fn list_command_filters_by_anchored_release_name() {
        let cmd =
            HelmCli::build_list_command("cilium", "kube-system", Path::new("/tmp/kubeconfig"));
        let args = argv(&cmd);
        for required in [
            "list",
            "--namespace",
            "kube-system",
            "--filter",
            "^cilium$",
            "--output",
            "json",
        ] {
            assert!(
                args.iter().any(|a| a == required),
                "missing {required}: {args:?}"
            );
        }
    }

    #[test]
    fn get_values_command_targets_release_as_json() {
        let cmd =
            HelmCli::build_get_values_command("argocd", "argocd", Path::new("/tmp/kubeconfig"));
        let args = argv(&cmd);
        for required in [
            "get",
            "values",
            "argocd",
            "--namespace",
            "argocd",
            "--output",
            "json",
        ] {
            assert!(
                args.iter().any(|a| a == required),
                "missing {required}: {args:?}"
            );
        }
    }

    #[test]
    fn parse_release_status_deployed() {
        let stdout =
            r#"[{"name":"cilium","namespace":"kube-system","status":"deployed","revision":"1"}]"#;
        assert_eq!(parse_release_status(stdout).as_deref(), Some("deployed"));
    }

    #[test]
    fn parse_release_status_failed() {
        let stdout = r#"[{"name":"argocd","status":"failed"}]"#;
        assert_eq!(parse_release_status(stdout).as_deref(), Some("failed"));
    }

    #[test]
    fn parse_release_status_empty_array_is_none() {
        assert_eq!(parse_release_status("[]"), None);
    }

    #[test]
    fn parse_release_status_garbage_is_none() {
        // A missing-release non-zero exit can leave stderr text /
        // empty stdout; non-JSON must map to None (→ install).
        assert_eq!(parse_release_status(""), None);
        assert_eq!(parse_release_status("Error: release: not found"), None);
    }

    #[test]
    fn parse_loader_hash_present() {
        let stdout = r#"{"apprafterLoaderHash":"abc123","ipv6":{"enabled":false}}"#;
        assert_eq!(parse_loader_hash(stdout).as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_loader_hash_absent_is_none() {
        let stdout = r#"{"ipv6":{"enabled":false}}"#;
        assert_eq!(parse_loader_hash(stdout), None);
    }

    #[test]
    fn parse_loader_hash_null_body_is_none() {
        // helm emits a literal `null` for a release with no user
        // values — must not panic, must map to None.
        assert_eq!(parse_loader_hash("null"), None);
    }
}
