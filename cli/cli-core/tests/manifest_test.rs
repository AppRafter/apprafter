// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for cli_core::manifest.
//!
//! Skips at runtime when `cue` is absent from PATH (mirrors the
//! cue_test pattern).

use std::path::{Path, PathBuf};

use cli_core::manifest::{self, InfrastructureManifest};
use cli_core::CliError;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

#[test]
fn parse_full_infrastructure_fixture() {
    let root = repo_root();
    let path = Path::new("./examples/infrastructure");

    let parsed: InfrastructureManifest = match manifest::parse_infrastructure(&root, path) {
        Ok(m) => m,
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected: {other}"),
    };

    assert_eq!(parsed.api_version, "apprafter.io/v1alpha1");
    assert_eq!(parsed.kind, "Infrastructure");
    assert_eq!(parsed.metadata.name, "platform-1");
    assert_eq!(parsed.spec.provider, "hetzner-cloud");
    assert_eq!(parsed.spec.region.as_deref(), Some("nbg1"));
    assert_eq!(parsed.spec.nodes.len(), 1);
    assert_eq!(parsed.spec.nodes[0].kind, "cpx22");
    assert_eq!(parsed.spec.nodes[0].count, 1);
    assert_eq!(parsed.spec.os_image.as_deref(), Some("ubuntu-24.04"));

    let net = parsed.spec.network.as_ref().expect("network present");
    assert_eq!(net.ip_range.as_deref(), Some("10.0.0.0/16"));
    let subnet = net.subnet.as_ref().expect("subnet present");
    assert_eq!(subnet.ip_range.as_deref(), Some("10.0.0.0/24"));
    assert_eq!(subnet.zone.as_deref(), Some("eu-central"));

    let fw = parsed.spec.firewall.as_ref().expect("firewall present");
    let ingress = fw.ingress.as_ref().expect("ingress present");
    // The fixture exposes three ports on the tier-1 firewall:
    // 22 (SSH bootstrap), 443 (HTTPS for HTTPRoute backends),
    // and 6443 (k3s apiserver for kubectl + helm from the
    // workstation — added in commit b9b204b). The assertion
    // was originally `== 2` and only surfaced as wrong once
    // the B.1.71 CI fix put cue on PATH so this test stopped
    // silent-skipping on Err(CueNotFound).
    assert_eq!(ingress.len(), 3);
    assert_eq!(ingress[0].port, "22");
    assert_eq!(ingress[0].protocol.as_deref(), Some("tcp"));
    assert_eq!(ingress[1].port, "443");
    assert_eq!(ingress[2].port, "6443");
    assert_eq!(ingress[2].protocol.as_deref(), Some("tcp"));
}

#[test]
fn parse_returns_error_on_missing_path() {
    let root = repo_root();
    let path = Path::new("./examples/infrastructure-does-not-exist");
    match manifest::parse_infrastructure(&root, path) {
        Err(CliError::CueExport { .. }) => {
            // Expected: cue export prints an error and exits non-zero.
        }
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("expected CueExport, got {other}"),
        Ok(_) => panic!("missing path should not parse successfully"),
    }
}

#[test]
fn parse_infrastructure_errors_when_no_infrastructure_document() {
    use cli_core::CliError;
    let dir = tempfile::tempdir().unwrap();
    let cue_path = dir.path().join("not-infra.cue");
    std::fs::write(
        &cue_path,
        "package x\n#Other: { kind: \"Other\" }\nout: #Other & { kind: \"Other\" }\n",
    )
    .unwrap();

    let err = cli_core::manifest::parse_infrastructure(dir.path(), &cue_path).unwrap_err();
    match err {
        CliError::CueNotFound => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        CliError::Other(msg) => {
            assert!(msg.contains("Infrastructure"), "{msg}");
        }
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn parse_infrastructure_decodes_optional_argocd_domain() {
    let dir = tempfile::tempdir().unwrap();
    let cue_path = dir.path().join("infra.cue");
    std::fs::write(
        &cue_path,
        "package x\n\
out: {\n\
    apiVersion: \"apprafter.io/v1alpha1\"\n\
    kind: \"Infrastructure\"\n\
    metadata: name: \"p\"\n\
    spec: {\n\
        provider: \"hetzner-cloud\"\n\
        argocd: domain: \"argo.example.com\"\n\
    }\n\
}\n",
    )
    .unwrap();

    let m = match cli_core::manifest::parse_infrastructure(dir.path(), &cue_path) {
        Ok(m) => m,
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected: {other}"),
    };
    let argocd = m.spec.argocd.expect("argocd block decoded");
    assert_eq!(argocd.domain.as_deref(), Some("argo.example.com"));
}

#[test]
fn parse_infrastructure_argocd_block_is_optional() {
    let dir = tempfile::tempdir().unwrap();
    let cue_path = dir.path().join("infra.cue");
    std::fs::write(
        &cue_path,
        "package x\n\
out: {\n\
    apiVersion: \"apprafter.io/v1alpha1\"\n\
    kind: \"Infrastructure\"\n\
    metadata: name: \"p\"\n\
    spec: provider: \"hetzner-cloud\"\n\
}\n",
    )
    .unwrap();

    let m = match cli_core::manifest::parse_infrastructure(dir.path(), &cue_path) {
        Ok(m) => m,
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected: {other}"),
    };
    assert!(m.spec.argocd.is_none());
}

#[test]
fn parse_infrastructure_decodes_argocd_bootstrap_repo_and_path() {
    let dir = tempfile::tempdir().unwrap();
    let cue_path = dir.path().join("infra.cue");
    std::fs::write(
        &cue_path,
        "package x\n\
out: {\n\
    apiVersion: \"apprafter.io/v1alpha1\"\n\
    kind: \"Infrastructure\"\n\
    metadata: name: \"p\"\n\
    spec: {\n\
        provider: \"hetzner-cloud\"\n\
        argocd: {\n\
            domain: \"argo.example.com\"\n\
            bootstrapRepo: \"https://github.com/acme/platform-state.git\"\n\
            bootstrapPath: \"clusters/prod\"\n\
        }\n\
    }\n\
}\n",
    )
    .unwrap();

    let m = match cli_core::manifest::parse_infrastructure(dir.path(), &cue_path) {
        Ok(m) => m,
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected: {other}"),
    };
    let argocd = m.spec.argocd.expect("argocd block decoded");
    assert_eq!(argocd.domain.as_deref(), Some("argo.example.com"));
    assert_eq!(
        argocd.bootstrap_repo.as_deref(),
        Some("https://github.com/acme/platform-state.git")
    );
    assert_eq!(argocd.bootstrap_path.as_deref(), Some("clusters/prod"));
}

#[test]
fn parse_infrastructure_decodes_optional_backstage_block() {
    let dir = tempfile::tempdir().unwrap();
    let cue_path = dir.path().join("infra.cue");
    std::fs::write(
        &cue_path,
        "package x\n\
out: {\n\
    apiVersion: \"apprafter.io/v1alpha1\"\n\
    kind: \"Infrastructure\"\n\
    metadata: name: \"p\"\n\
    spec: {\n\
        provider: \"hetzner-cloud\"\n\
        backstage: {\n\
            domain: \"backstage.example.com\"\n\
            image:  \"ghcr.io/acme/backstage:1.42.0\"\n\
        }\n\
    }\n\
}\n",
    )
    .unwrap();

    let m = match cli_core::manifest::parse_infrastructure(dir.path(), &cue_path) {
        Ok(m) => m,
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected: {other}"),
    };
    let bs = m.spec.backstage.expect("backstage block decoded");
    assert_eq!(bs.domain.as_deref(), Some("backstage.example.com"));
    assert_eq!(bs.image.as_deref(), Some("ghcr.io/acme/backstage:1.42.0"));
}

#[test]
fn parse_infrastructure_decodes_optional_admission_webhook_block() {
    let dir = tempfile::tempdir().unwrap();
    let cue_path = dir.path().join("infra.cue");
    std::fs::write(
        &cue_path,
        "package x\n\
out: {\n\
    apiVersion: \"apprafter.io/v1alpha1\"\n\
    kind: \"Infrastructure\"\n\
    metadata: name: \"p\"\n\
    spec: {\n\
        provider: \"hetzner-cloud\"\n\
        admissionWebhook: image: \"ghcr.io/acme/admission-webhook:1.0.0\"\n\
    }\n\
}\n",
    )
    .unwrap();

    let m = match cli_core::manifest::parse_infrastructure(dir.path(), &cue_path) {
        Ok(m) => m,
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected: {other}"),
    };
    let aw = m
        .spec
        .admission_webhook
        .expect("admission_webhook block decoded");
    assert_eq!(
        aw.image.as_deref(),
        Some("ghcr.io/acme/admission-webhook:1.0.0")
    );
}
