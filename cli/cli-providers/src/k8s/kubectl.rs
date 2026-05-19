// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Wrapper around the `kubectl` binary. Real impl shells out;
//! tests inject fakes via the `KubectlRunner` trait.

use std::path::{Path, PathBuf};
use std::process::Command;

use cli_core::{CliError, Result};

/// Pinned Gateway API release. Bumped in lockstep with the v1.4
/// CRD admission work that lives in plan.md sub-phase 1.7.
pub const GATEWAY_API_VERSION: &str = "v1.2.1";

/// Field manager identity used by `apprafter`-driven
/// server-side `kubectl apply` calls. Mirrors the operator's
/// `apprafter-operator` field manager so cluster-side ownership
/// is clear from `metadata.managedFields`.
pub const APPRAFTER_CLI_FIELD_MANAGER: &str = "apprafter-cli";

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

    /// Apply a manifest using **server-side apply** with the
    /// supplied field manager. MUST be used for manifests carrying
    /// sensitive data (e.g. Secrets emitted via `stringData:`).
    /// Server-side apply tracks ownership in
    /// `metadata.managedFields` instead of the
    /// `kubectl.kubernetes.io/last-applied-configuration`
    /// annotation, which otherwise stores the entire applied
    /// manifest as plaintext JSON (= raw-token leak for Secrets
    /// with `stringData`). `--force-conflicts` is passed so
    /// resources that pre-exist with client-side ownership migrate
    /// cleanly to the new field manager on first SSA apply.
    fn apply_manifest_server_side(
        &self,
        source: &ManifestSource,
        kubeconfig_path: &Path,
        field_manager: &str,
    ) -> Result<()>;

    /// Read a single key from a Kubernetes Secret and return its
    /// base64-decoded contents as a UTF-8 string. Used by
    /// `apprafter argocd-password` to retrieve the
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

    fn build_apply_server_side_command(
        source: &ManifestSource,
        kubeconfig_path: &Path,
        field_manager: &str,
    ) -> Command {
        let mut c = Command::new("kubectl");
        c.arg("apply")
            .arg("--server-side")
            .arg(format!("--field-manager={field_manager}"))
            .arg("--force-conflicts")
            .arg("-f");
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

    fn apply_manifest_server_side(
        &self,
        source: &ManifestSource,
        kubeconfig_path: &Path,
        field_manager: &str,
    ) -> Result<()> {
        let status = Self::build_apply_server_side_command(source, kubeconfig_path, field_manager)
            .status()
            .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;
        if !status.success() {
            return Err(CliError::Other(format!(
                "kubectl apply --server-side --field-manager={field_manager} -f failed (exit {:?})",
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
    fn apply_server_side_command_passes_field_manager_and_force_conflicts() {
        // Regression guard for v0.1.68 fix: secret-bearing manifests
        // MUST use server-side apply so the raw stringData payload
        // does not leak into the
        // `kubectl.kubernetes.io/last-applied-configuration`
        // annotation. The command must include
        // `--server-side`, `--field-manager=apprafter-cli`, and
        // `--force-conflicts` (so an existing client-side-applied
        // resource can migrate ownership cleanly on first SSA run).
        let cmd = KubectlCli::build_apply_server_side_command(
            &ManifestSource::Path("/tmp/secret.yaml".into()),
            Path::new("/tmp/kubeconfig"),
            APPRAFTER_CLI_FIELD_MANAGER,
        );
        let args = argv(&cmd);
        for required in [
            "apply",
            "--server-side",
            "--field-manager=apprafter-cli",
            "--force-conflicts",
            "-f",
            "/tmp/secret.yaml",
        ] {
            assert!(
                args.iter().any(|a| a == required),
                "missing {required}: {args:?}"
            );
        }
        // Defensive: the legacy client-side path must NOT appear.
        assert!(!args.iter().any(|a| a == "--client-side"), "{args:?}");
    }

    #[test]
    fn apply_server_side_command_with_url_source_still_emits_ssa_flags() {
        let cmd = KubectlCli::build_apply_server_side_command(
            &ManifestSource::Url("https://example.com/secret.yaml".into()),
            Path::new("/tmp/kubeconfig"),
            "custom-fm",
        );
        let args = argv(&cmd);
        for required in [
            "apply",
            "--server-side",
            "--field-manager=custom-fm",
            "--force-conflicts",
            "-f",
            "https://example.com/secret.yaml",
        ] {
            assert!(
                args.iter().any(|a| a == required),
                "missing {required}: {args:?}"
            );
        }
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
