// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Wrapper around the `helm` binary. Real impl shells out;
//! tests inject fakes via the `HelmRunner` trait.

use std::path::PathBuf;
use std::process::Command;

use cli_core::{CliError, Result};

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
}

pub trait HelmRunner {
    fn repo_add(&self, name: &str, url: &str) -> Result<()>;
    fn upgrade_install(&self, args: &HelmUpgradeArgs) -> Result<()>;
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
        c
    }
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
        });
        let args = argv(&cmd);
        assert!(!args.iter().any(|a| a == "--version"), "{args:?}");
        // The rest of the flags must still be present.
        assert!(args.iter().any(|a| a == "--create-namespace"), "{args:?}");
        assert!(args.iter().any(|a| a == "--wait"), "{args:?}");
    }
}
