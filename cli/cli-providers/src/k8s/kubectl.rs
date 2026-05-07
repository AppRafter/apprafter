// SPDX-License-Identifier: FSL-1.1-MIT
//! Wrapper around the `kubectl` binary. Real impl shells out;
//! tests inject fakes via the `KubectlRunner` trait.

use std::path::{Path, PathBuf};
use std::process::Command;

use cli_core::{CliError, Result};

/// Pinned Gateway API release. Bumped in lockstep with the v1.4
/// CRD admission work that lives in plan.md sub-phase 1.7.
pub const GATEWAY_API_VERSION: &str = "v1.2.1";

/// URL of the upstream "standard install" YAML — Gateway, HTTPRoute,
/// GRPCRoute, ReferenceGrant CRDs (the conformance baseline).
pub fn gateway_api_crds_url() -> String {
    format!(
        "https://github.com/kubernetes-sigs/gateway-api/releases/download/{GATEWAY_API_VERSION}/standard-install.yaml"
    )
}

#[derive(Debug, Clone)]
pub enum ManifestSource {
    Url(String),
    Path(PathBuf),
}

pub trait KubectlRunner {
    fn apply_manifest(&self, source: &ManifestSource, kubeconfig_path: &Path) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct KubectlCli;

impl KubectlCli {
    fn build_apply_command(source: &ManifestSource, kubeconfig_path: &Path) -> Command {
        let mut c = Command::new("kubectl");
        c.arg("apply").arg("-f");
        match source {
            ManifestSource::Url(u) => {
                c.arg(u);
            }
            ManifestSource::Path(p) => {
                c.arg(p);
            }
        }
        c.env("KUBECONFIG", kubeconfig_path);
        c
    }
}

impl KubectlRunner for KubectlCli {
    fn apply_manifest(&self, source: &ManifestSource, kubeconfig_path: &Path) -> Result<()> {
        let status = Self::build_apply_command(source, kubeconfig_path)
            .status()
            .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
        if !status.success() {
            return Err(CliError::Other(format!(
                "kubectl apply -f failed (exit {:?})",
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
    fn apply_url_command_passes_minus_f_with_the_url() {
        let cmd = KubectlCli::build_apply_command(
            &ManifestSource::Url("https://example.com/x.yaml".into()),
            Path::new("/tmp/kubeconfig"),
        );
        let args = argv(&cmd);
        assert_eq!(args, vec!["apply", "-f", "https://example.com/x.yaml"]);
    }

    #[test]
    fn apply_path_command_passes_minus_f_with_the_path() {
        let cmd = KubectlCli::build_apply_command(
            &ManifestSource::Path("/tmp/m.yaml".into()),
            Path::new("/tmp/kubeconfig"),
        );
        let args = argv(&cmd);
        assert_eq!(args, vec!["apply", "-f", "/tmp/m.yaml"]);
    }

    #[test]
    fn gateway_api_crds_url_points_at_standard_install_for_pinned_version() {
        let url = gateway_api_crds_url();
        assert!(url.contains(GATEWAY_API_VERSION), "{url}");
        assert!(url.ends_with("/standard-install.yaml"), "{url}");
        assert!(url.starts_with("https://"), "{url}");
    }
}
