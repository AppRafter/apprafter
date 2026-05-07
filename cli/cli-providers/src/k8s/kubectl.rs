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
    /// Read a single key from a Kubernetes Secret and return its
    /// base64-decoded contents as a UTF-8 string. Used by
    /// `platform-cli argocd-password` to retrieve the
    /// `argocd-initial-admin-secret` value.
    fn get_secret_value(
        &self,
        secret_name: &str,
        namespace: &str,
        key: &str,
        kubeconfig_path: &Path,
    ) -> Result<String>;
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

    fn build_get_secret_command(
        secret_name: &str,
        namespace: &str,
        key: &str,
        kubeconfig_path: &Path,
    ) -> Command {
        let mut c = Command::new("kubectl");
        c.arg("get")
            .arg("secret")
            .arg(secret_name)
            .arg("-n")
            .arg(namespace)
            .arg("-o")
            .arg(format!("jsonpath={{.data.{key}}}"))
            .env("KUBECONFIG", kubeconfig_path);
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

    fn get_secret_value(
        &self,
        secret_name: &str,
        namespace: &str,
        key: &str,
        kubeconfig_path: &Path,
    ) -> Result<String> {
        use base64::Engine;
        let output = Self::build_get_secret_command(secret_name, namespace, key, kubeconfig_path)
            .output()
            .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(CliError::Other(format!(
                "kubectl get secret {secret_name} -n {namespace} -o jsonpath failed (exit {:?}): {stderr}",
                output.status.code()
            )));
        }
        let b64 = String::from_utf8(output.stdout)
            .map_err(|e| CliError::Other(format!("kubectl stdout not utf-8: {e}")))?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| {
                CliError::Other(format!(
                    "decode secret {secret_name}/{key} (raw bytes were not valid base64): {e}"
                ))
            })?;
        String::from_utf8(decoded)
            .map_err(|e| CliError::Other(format!("secret {secret_name}/{key} is not utf-8: {e}")))
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

    #[test]
    fn get_secret_command_uses_jsonpath_for_the_data_key() {
        let cmd = KubectlCli::build_get_secret_command(
            "argocd-initial-admin-secret",
            "argocd",
            "password",
            Path::new("/tmp/kubeconfig"),
        );
        let args = argv(&cmd);
        assert_eq!(
            args,
            vec![
                "get",
                "secret",
                "argocd-initial-admin-secret",
                "-n",
                "argocd",
                "-o",
                "jsonpath={.data.password}",
            ]
        );
    }
}
