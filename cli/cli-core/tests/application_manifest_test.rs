// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Integration tests for `cli_core::manifest::parse_application`.
//!
//! Panics with a clear "install cue / run from `nix develop`"
//! message if the `cue` binary is missing from PATH. The earlier
//! pattern of silent-skipping on `CueNotFound` was changed after
//! a v0.1.109 incident where a fixture / assertion mismatch sat
//! undetected for 5+ commits because every local `cargo test`
//! silently skipped without cue.

use std::path::{Path, PathBuf};

use cli_core::manifest::{self, ApplicationManifest, EnvValue};
use cli_core::CliError;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

#[test]
fn parse_full_application_fixture() {
    let root = repo_root();
    let path = Path::new("./examples/applications");

    // `examples/applications` is a multi-app CUE package (parser, redisApp, a
    // needs-array demo, …). Select the `parser` reference app BY NAME rather
    // than relying on `cue export` key ordering — a new example whose binding
    // sorts before `parser` must not silently change which doc this asserts on
    // (the v0.1.109-class drift this test guards against).
    let value = match cli_core::cue::export_in(&root, path) {
        Ok(v) => v,
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected: {other}"),
    };
    let parser_doc = value
        .as_object()
        .expect("cue export yields a JSON object")
        .values()
        .find(|c| {
            c.get("kind").and_then(serde_json::Value::as_str) == Some("Application")
                && c.pointer("/metadata/name")
                    .and_then(serde_json::Value::as_str)
                    == Some("parser")
        })
        .expect("parser Application fixture present in examples/applications");
    let parsed: ApplicationManifest =
        serde_json::from_value(parser_doc.clone()).expect("parser fixture decodes");

    assert_eq!(parsed.api_version, "apprafter.io/v1alpha1");
    assert_eq!(parsed.kind, "Application");
    assert_eq!(parsed.metadata.name, "parser");

    let base = parsed.spec.base.expect("base block decoded");
    assert_eq!(base.image.as_deref(), Some("ghcr.io/example/parser:latest"));
    assert_eq!(base.replicas, Some(3));

    let expose = base.expose.expect("expose decoded");
    assert_eq!(expose.port, Some(8080));
    assert_eq!(expose.network.as_deref(), None);

    let env = base.env.expect("env decoded");
    assert!(
        matches!(env.get("LOG_LEVEL"), Some(EnvValue::Literal(s)) if s == "info"),
        "LOG_LEVEL literal must decode to EnvValue::Literal(\"info\")"
    );

    let envs = parsed.spec.environments.expect("environments decoded");
    let dev = envs.get("dev").expect("dev override present");
    assert_eq!(dev.replicas, Some(1));
    let dev_expose = dev.expose.as_ref().expect("dev expose present");
    assert_eq!(dev_expose.network.as_deref(), Some("vpn"));

    let prod = envs.get("prod").expect("prod override present");
    assert_eq!(prod.replicas, Some(3));
}

#[test]
fn parse_application_returns_error_on_missing_path() {
    let root = repo_root();
    let path = Path::new("./examples/applications-does-not-exist");
    match manifest::parse_application(&root, path) {
        Err(CliError::CueExport { .. }) => {}
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
fn parse_application_errors_when_no_application_document() {
    let dir = tempfile::tempdir().unwrap();
    let cue_path = dir.path().join("not-app.cue");
    std::fs::write(
        &cue_path,
        "package x\n#Other: { kind: \"Other\" }\nout: #Other & { kind: \"Other\" }\n",
    )
    .unwrap();

    let err = manifest::parse_application(dir.path(), &cue_path).unwrap_err();
    match err {
        CliError::CueNotFound => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        CliError::Other(msg) => {
            assert!(msg.contains("Application"), "{msg}");
        }
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn parse_application_decodes_minimal_manifest_without_environments() {
    // The base block alone (no environments map) is a valid
    // v1alpha1 Application — the parser must accept it.
    let dir = tempfile::tempdir().unwrap();
    let cue_path = dir.path().join("app.cue");
    std::fs::write(
        &cue_path,
        "package x\n\
out: {\n\
    apiVersion: \"apprafter.io/v1alpha1\"\n\
    kind: \"Application\"\n\
    metadata: name: \"web\"\n\
    spec: {\n\
        base: {\n\
            image:    \"ghcr.io/acme/web:1.0\"\n\
            replicas: 1\n\
        }\n\
    }\n\
}\n",
    )
    .unwrap();

    let m = match manifest::parse_application(dir.path(), &cue_path) {
        Ok(m) => m,
        Err(CliError::CueNotFound) => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(other) => panic!("unexpected: {other}"),
    };
    assert_eq!(m.kind, "Application");
    let base = m.spec.base.expect("base decoded");
    assert_eq!(base.image.as_deref(), Some("ghcr.io/acme/web:1.0"));
    assert!(m.spec.environments.is_none());
}

#[test]
fn application_schema_vets_cleanly() {
    use std::process::Command;

    let bin = std::env::var("CUE_BIN").unwrap_or_else(|_| "cue".to_string());
    let root = repo_root();

    let output = match Command::new(&bin)
        .current_dir(&root)
        .args(["vet", "./schemas/v1alpha1/..."])
        .output()
    {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(err) => panic!("spawn cue vet: {err}"),
    };
    assert!(
        output.status.success(),
        "cue vet failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn application_fixture_vets_against_schema() {
    use std::process::Command;

    let bin = std::env::var("CUE_BIN").unwrap_or_else(|_| "cue".to_string());
    let root = repo_root();

    let output = match Command::new(&bin)
        .current_dir(&root)
        .args([
            "vet",
            "-c",
            "./schemas/v1alpha1/...",
            "./examples/applications/parser.cue",
        ])
        .output()
    {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            panic!(
                "cue must be on PATH for this test — run from \
                 `nix develop` or install cue v0.10+"
            );
        }
        Err(err) => panic!("spawn cue vet: {err}"),
    };
    assert!(
        output.status.success(),
        "cue vet of the parser fixture failed:\nstderr={}\nstdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

/// An `expose` block that sets only `network` must decode.
///
/// This is legal everywhere else in the stack and is what a per-environment
/// override looks like when it changes visibility and nothing else: `cue vet`
/// accepts it, `apprafter app validate` reports valid, the admission webhook
/// returns no errors, and the operator merges the port from `base`. The CLI
/// was the one layer that refused it — `ApplicationExpose::port` was a
/// REQUIRED serde field, so `docs-site/apprafter/Application.cue`'s
///
///     environments: dev: expose: network: "internal"
///
/// failed to deserialise with `missing field `port``. The only caller of that
/// parse is `app add`'s picker setup, which warns and then hides the
/// environment picker and skips the namespace preselect — so a manifest the
/// whole rest of the platform accepts silently lost two pieces of the wizard.
///
/// The shape is asserted here rather than in a fixture file because it is a
/// DESERIALISATION contract: the CLI only ever has to read every shape the
/// schema permits, never to produce them.
#[test]
fn expose_override_without_port_decodes() {
    let doc = serde_json::json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "Application",
        "metadata": {"name": "docs", "namespace": "apprafter"},
        "spec": {
            "base": {
                "image": "ghcr.io/apprafter/docs:latest",
                "replicas": 2,
                "expose": {"port": 80, "network": "public", "hostname": "docs.example.dev"}
            },
            "environments": {
                // The override under test: visibility only, port inherited.
                "dev": {"replicas": 1, "expose": {"network": "internal"}}
            }
        }
    });

    let parsed: ApplicationManifest = serde_json::from_value(doc)
        .expect("an expose override carrying only `network` must decode");

    let base_expose = parsed
        .spec
        .base
        .expect("base decoded")
        .expose
        .expect("base expose decoded");
    assert_eq!(
        base_expose.port,
        Some(80),
        "a port that IS present must still decode"
    );

    let envs = parsed.spec.environments.expect("environments decoded");
    let dev_expose = envs
        .get("dev")
        .expect("dev environment decoded")
        .expose
        .as_ref()
        .expect("dev expose decoded");
    assert_eq!(
        dev_expose.port, None,
        "an absent port must decode as None, not fail the whole manifest"
    );
    assert_eq!(dev_expose.network.as_deref(), Some("internal"));
}
