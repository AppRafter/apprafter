// SPDX-License-Identifier: FSL-1.1-MIT
//! Embeds `operator/charts/apprafter-operator/` into the
//! `cli-providers` crate at compile time and exposes a runtime
//! extractor that materialises it under a fresh tempdir so
//! `helm upgrade --install apprafter-operator <tempdir>` can run.
//!
//! Using `include_dir!` keeps `platform-cli` installable as a
//! standalone binary — operators don't need the AppRafter repo on
//! disk to bootstrap a cluster.

use std::path::{Path, PathBuf};

use cli_core::{CliError, Result};
use include_dir::{include_dir, Dir};
use tempfile::TempDir;

/// Helm release name used for the operator install. Both
/// `cluster-bootstrap` and `destroy --yes` reference this to
/// uninstall / reinstall idempotently.
pub const APPRAFTER_OPERATOR_RELEASE_NAME: &str = "apprafter-operator";

/// Compile-time-embedded copy of the operator helm chart.
/// Path is relative to `cli/cli-providers/Cargo.toml`.
static OPERATOR_CHART_DIR: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/../../operator/charts/apprafter-operator");

/// Extract the embedded chart files into a fresh tempdir, returning
/// the tempdir handle (RAII — caller keeps it alive) and the
/// chart-root path under it.
pub fn extract_operator_chart_to_tempdir() -> Result<(TempDir, PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix("apprafter-operator-chart-")
        .tempdir()
        .map_err(|e| CliError::Other(format!("create chart tempdir: {e}")))?;
    let root = dir.path().to_path_buf();
    write_dir_recursive(&OPERATOR_CHART_DIR, &root)?;
    Ok((dir, root))
}

fn write_dir_recursive(src: &Dir<'_>, dst: &Path) -> Result<()> {
    for entry in src.entries() {
        match entry {
            include_dir::DirEntry::Dir(sub) => {
                let sub_path = dst.join(
                    sub.path()
                        .file_name()
                        .ok_or_else(|| CliError::Other("chart entry without name".into()))?,
                );
                std::fs::create_dir_all(&sub_path)
                    .map_err(|e| CliError::Other(format!("mkdir {}: {e}", sub_path.display())))?;
                write_dir_recursive(sub, &sub_path)?;
            }
            include_dir::DirEntry::File(file) => {
                let name = file
                    .path()
                    .file_name()
                    .ok_or_else(|| CliError::Other("chart file without name".into()))?;
                let path = dst.join(name);
                std::fs::write(&path, file.contents()).map_err(|e| {
                    CliError::Other(format!("write {}: {e}", path.display()))
                })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_chart_contains_chart_yaml_and_templates() {
        let (tempdir, root) = extract_operator_chart_to_tempdir().expect("extract");
        let chart_yaml = root.join("Chart.yaml");
        assert!(chart_yaml.exists(), "missing Chart.yaml at {}", root.display());
        let body = std::fs::read_to_string(&chart_yaml).unwrap();
        assert!(body.contains("name: apprafter-operator"), "{body}");

        let templates = root.join("templates");
        assert!(templates.is_dir(), "missing templates/ dir");
        assert!(templates.join("deployment.yaml").exists());
        assert!(templates.join("rbac.yaml").exists());

        // Keep the tempdir alive until end of scope.
        drop(tempdir);
    }

    #[test]
    fn release_name_pins_apprafter_operator() {
        assert_eq!(APPRAFTER_OPERATOR_RELEASE_NAME, "apprafter-operator");
    }
}
