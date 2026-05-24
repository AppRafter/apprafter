// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter app open <name>` — port-forward a registered
//! Application and open the browser. Track B.1.79b first
//! deliverable.
//!
//! Resolves chain:
//!
//!   1. Argo CD Application CR `<name>` in `argocd` namespace
//!      (load-bearing — refuses if missing с pointer к
//!      `apprafter app list`).
//!   2. `spec.destination.namespace` = the namespace where
//!      Argo CD lays down children.
//!   3. AppRafter Application name through Argo CD's
//!      `status.resources[]` — first entry с `group:
//!      "apprafter.io", kind: "Application"`. Walk-fix #1
//!      post-B.1.79b: Argo CD only stamps the standard
//!      `app.kubernetes.io/instance=<argocd-app-name>` label
//!      on resources it directly applies. Service +
//!      Deployment for AppRafter apps are rendered by the
//!      operator from the Application CR (second-level
//!      ownership), so they carry the operator's labels
//!      (`app.kubernetes.io/name=<apprafter-app-name>`,
//!      `apprafter=true`, `managed-by=apprafter-operator`)
//!      keyed off the **inner** AppRafter Application's
//!      `metadata.name`, not the outer Argo CD parent's
//!      name.
//!   4. Service in destination namespace via
//!      `app.kubernetes.io/name=<apprafter-app-name>`.
//!      Single-result is happy path; multi-match errors с
//!      candidate enumeration.
//!   5. Container port: `--container-port` override >
//!      Service's first `spec.ports[].port`.
//!   6. Local port: `--port` override > 8080 with auto-
//!      increment к 8090 if busy (probe via TcpListener::bind
//!      on 127.0.0.1).
//!
//! Once the forward is bound, prints a banner + opens the
//! default browser (unless `--no-browser`), then blocks on
//! the kubectl child. Ctrl+C tears down through the process
//! group; the drainer threads in `commands::port_forward`
//! keep the pipes flowing so Go SIGPIPE doesn't kill kubectl
//! prematurely.

use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

use cli_core::{CliError, Result};
use inquire::Confirm;
use serde_json::Value;

use crate::commands::k8s_helpers::{ensure_kubeconfig_tempfile, kubectl_get_json};
use crate::commands::port_forward::{open_in_browser, spawn_kubectl_port_forward, wait_ready};

const ARGOCD_NAMESPACE: &str = "argocd";
const DEFAULT_LOCAL_PORT: u16 = 8080;
const LOCAL_PORT_MAX_PROBES: u16 = 10;

/// Resolved service surface returned by the kubectl listing.
/// Pulled out so tests can exercise selection and port
/// resolution against fixture JSON без cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceInfo {
    pub name: String,
    /// First-port-wins for the container target. Empty when
    /// the Service declares no ports — caller rejects.
    pub ports: Vec<u16>,
}

/// Entry point for `apprafter app open <name>`.
pub fn open(
    name: &str,
    local_port: Option<u16>,
    container_port: Option<u16>,
    no_browser: bool,
) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    let app = kubectl_get_json(
        "application.argoproj.io",
        Some(name),
        Some(ARGOCD_NAMESPACE),
        kc.path(),
    )?
    .ok_or_else(|| {
        CliError::Other(format!(
            "Application '{name}' not found in namespace {ARGOCD_NAMESPACE}. \
             List with `apprafter app list`."
        ))
    })?;

    let dest_ns = app
        .pointer("/spec/destination/namespace")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::Other(format!(
                "Application '{name}' has no `spec.destination.namespace` — \
                 cannot determine forward target. Inspect with `apprafter app status {name}`."
            ))
        })?
        .to_string();

    confirm_or_proceed_on_unhealthy(&app, name)?;

    let apprafter_app_name = find_apprafter_app_name(&app).ok_or_else(|| {
        CliError::Other(format!(
            "Could not determine the AppRafter Application name from Argo CD's \
             `status.resources[]` for '{name}'. The Argo CD Application's CMP \
             output should declare exactly one `apprafter.io/Application` \
             resource. If your manifest renders only raw Kubernetes resources \
             (Deployment/Service direct), `app open` doesn't know which \
             Service is primary yet — port-forward manually until walk-fix \
             support for non-AppRafter apps lands."
        ))
    })?;

    let services = list_services_for_apprafter_app(&apprafter_app_name, &dest_ns, kc.path())?;
    let service = select_service(&services, &apprafter_app_name)?;

    let resolved_container_port = resolve_container_port(container_port, &service)?;
    let resolved_local_port = pick_local_port(local_port.unwrap_or(DEFAULT_LOCAL_PORT))?;

    let target = format!("svc/{}", service.name);
    let mut child = spawn_kubectl_port_forward(
        &target,
        &dest_ns,
        resolved_local_port,
        resolved_container_port,
        kc.path(),
    )?;
    wait_ready(&mut child)?;

    let url = format!("http://localhost:{resolved_local_port}");
    println!();
    println!("Forwarding to {name} on namespace '{dest_ns}'...");
    println!("  Service:   {}", service.name);
    println!(
        "  Port:      {resolved_container_port} (container) → {resolved_local_port} (localhost)"
    );
    println!("  URL:       {url}");
    println!();

    if no_browser {
        println!("ℹ Browser open skipped (--no-browser)");
    } else {
        match open_in_browser(&url) {
            Ok(()) => println!("✓ Browser opened"),
            Err(_) => println!("ℹ Browser open failed — paste the URL into your browser"),
        }
    }
    println!("ℹ Press Ctrl+C to stop port-forward");

    let _ = child.wait();
    Ok(())
}

/// Argo CD Application's `status.sync.status` + `status.
/// health.status`. If health is anything other than `Healthy`,
/// warn + prompt. Operator may legitimately want к forward
/// against а Progressing app (debugging а stuck rollout), so
/// we ask rather than refuse — но print the actual state so
/// they know what they're getting.
fn confirm_or_proceed_on_unhealthy(app: &Value, name: &str) -> Result<()> {
    let sync_state = app
        .pointer("/status/sync/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let health_state = app
        .pointer("/status/health/status")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");

    if health_state == "Healthy" {
        return Ok(());
    }

    eprintln!(
        "⚠ Application '{name}' is sync={sync_state}, health={health_state}. \
         Port-forward may fail or hit а partial deployment."
    );

    match Confirm::new("Continue anyway?")
        .with_default(false)
        .prompt()
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(CliError::Other("aborted by operator".into())),
        // Non-interactive shell или Ctrl+C at the prompt — refuse safely.
        Err(_) => Err(CliError::Other(
            "Application is not Healthy; rerun in а TTY to confirm, or pass \
             a healthy app name."
                .into(),
        )),
    }
}

/// Pure helper — walk Argo CD's `status.resources[]` to find
/// the AppRafter Application CR it owns. Returns the inner
/// `metadata.name` (which is the value the operator uses for
/// `app.kubernetes.io/name` labels на rendered Service +
/// Deployment) or `None` if no `apprafter.io/Application`
/// entry exists в the array (raw-helm/kustomize apps без а
/// CMP-rendered AppRafter CR).
///
/// Walk-fix #1 post-B.1.79b: original v0.1.161 implementation
/// keyed off Argo CD's standard `app.kubernetes.io/instance`
/// label, but Argo CD only stamps that on resources it
/// directly applies — operator-rendered children carry а
/// different label set. This helper bridges from outer (Argo
/// CD) к inner (AppRafter) naming.
pub(crate) fn find_apprafter_app_name(argocd_app: &Value) -> Option<String> {
    let resources = argocd_app.pointer("/status/resources")?.as_array()?;
    for r in resources {
        let group = r.get("group").and_then(Value::as_str).unwrap_or("");
        let kind = r.get("kind").and_then(Value::as_str).unwrap_or("");
        if group == "apprafter.io" && kind == "Application" {
            return r.get("name").and_then(Value::as_str).map(String::from);
        }
    }
    None
}

/// Shell out к `kubectl get svc -n <ns> -l app.kubernetes.io/
/// name=<apprafter-app-name> -o json`. The label is stamped
/// by `operator-rendering::make_labels`; its value is the
/// AppRafter Application CR's `metadata.name` (NOT the Argo
/// CD parent name) — see `find_apprafter_app_name` for the
/// mapping.
fn list_services_for_apprafter_app(
    apprafter_app_name: &str,
    namespace: &str,
    kubeconfig: &Path,
) -> Result<Vec<ServiceInfo>> {
    let selector = format!("app.kubernetes.io/name={apprafter_app_name}");
    let out = Command::new("kubectl")
        .arg("get")
        .arg("svc")
        .arg("-n")
        .arg(namespace)
        .arg("-l")
        .arg(&selector)
        .arg("-o")
        .arg("json")
        .env("KUBECONFIG", kubeconfig)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl get svc: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl get svc failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let parsed: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
    Ok(parse_services(&parsed))
}

/// Pure helper — extract `ServiceInfo` entries from а `kubectl
/// get svc -o json` payload. Tests drive this directly with
/// fixture JSON.
pub(crate) fn parse_services(payload: &Value) -> Vec<ServiceInfo> {
    let items = payload.get("items").and_then(Value::as_array);
    let Some(items) = items else { return vec![] };
    items
        .iter()
        .filter_map(|item| {
            let name = item
                .pointer("/metadata/name")
                .and_then(Value::as_str)?
                .to_string();
            let ports = item
                .pointer("/spec/ports")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| p.get("port").and_then(Value::as_u64))
                        .filter_map(|n| u16::try_from(n).ok())
                        .collect::<Vec<u16>>()
                })
                .unwrap_or_default();
            Some(ServiceInfo { name, ports })
        })
        .collect()
}

/// Pick the Service to forward. Plan §1.79b says the wizard
/// uses Argo CD's instance label; if exactly one Service
/// matches, we're done. Zero matches → fall through к а direct
/// `svc/<app-name>` lookup in а follow-up walk-fix (for now —
/// error with hint). Multi-match → error with names listed.
pub(crate) fn select_service(
    services: &[ServiceInfo],
    apprafter_app_name: &str,
) -> Result<ServiceInfo> {
    match services.len() {
        0 => Err(CliError::Other(format!(
            "No Service found in the destination namespace with label \
             `app.kubernetes.io/name={apprafter_app_name}` (the value comes \
             from the AppRafter Application CR's `metadata.name`, stamped by \
             the operator on every rendered Service / Deployment). Possible \
             causes: (a) operator hasn't yet rendered the Service — check \
             `kubectl get applications.apprafter.io -n <dest-ns>` and \
             `kubectl logs -n apprafter-system deployment/apprafter-operator`; \
             (b) the Application CR's `spec.expose` block is omitted, so no \
             Service is rendered (Deployment-only flow — no port-forward \
             target); (c) the label was overridden by а manifest's \
             `metadata.labels` block on the AppRafter CR."
        ))),
        1 => Ok(services[0].clone()),
        _ => {
            let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
            Err(CliError::Other(format!(
                "Multiple Services match label `app.kubernetes.io/name=\
                 {apprafter_app_name}`: {}. Disambiguation by Service name is \
                 а future walk-fix; for now, port-forward manually: \
                 `kubectl port-forward svc/<name> -n <ns> 8080:<port>`.",
                names.join(", ")
            )))
        }
    }
}

/// Pure helper — resolve the container port к target. Operator
/// override wins; otherwise first declared port on the
/// Service. Empty `ports` (Service declares none) is rejected
/// with а clear hint.
pub(crate) fn resolve_container_port(
    override_port: Option<u16>,
    service: &ServiceInfo,
) -> Result<u16> {
    if let Some(p) = override_port {
        return Ok(p);
    }
    service.ports.first().copied().ok_or_else(|| {
        CliError::Other(format!(
            "Service '{}' declares no `spec.ports` entries; pass \
             `--container-port <N>` to specify which port to forward.",
            service.name
        ))
    })
}

/// Probe-bind а TcpListener to 127.0.0.1:port; if it binds,
/// the port is free (we drop the listener immediately).
/// kubectl's actual bind happens shortly after, so the
/// EADDRINUSE race window is tiny but non-zero; if it loses
/// the race, the operator sees the kubectl error and reruns
/// with `--port <N>`.
fn pick_local_port(start: u16) -> Result<u16> {
    for offset in 0..LOCAL_PORT_MAX_PROBES {
        let candidate = start.saturating_add(offset);
        if is_port_free(candidate) {
            return Ok(candidate);
        }
    }
    Err(CliError::Other(format!(
        "No free local port between {start} and {}. Pass `--port <N>` to \
         specify а different starting point.",
        start.saturating_add(LOCAL_PORT_MAX_PROBES.saturating_sub(1))
    )))
}

fn is_port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_services_extracts_name_and_ports() {
        // Two Services with single ports each — happy multi-
        // entry case; tests the iterator and port casting.
        let payload = json!({
            "items": [
                {
                    "metadata": { "name": "landing-web" },
                    "spec": { "ports": [{ "port": 80, "name": "http" }] }
                },
                {
                    "metadata": { "name": "landing-cms" },
                    "spec": { "ports": [{ "port": 3000, "name": "http" }] }
                }
            ]
        });
        let svcs = parse_services(&payload);
        assert_eq!(svcs.len(), 2);
        assert_eq!(svcs[0].name, "landing-web");
        assert_eq!(svcs[0].ports, vec![80]);
        assert_eq!(svcs[1].ports, vec![3000]);
    }

    #[test]
    fn parse_services_handles_multi_port_service() {
        // Multi-port Service (e.g. http + grpc on one
        // Service object) — ports list preserves order so
        // `resolve_container_port` defaults to the first
        // declared port.
        let payload = json!({
            "items": [{
                "metadata": { "name": "api" },
                "spec": { "ports": [
                    { "port": 8080, "name": "http" },
                    { "port": 9090, "name": "grpc" }
                ]}
            }]
        });
        let svcs = parse_services(&payload);
        assert_eq!(svcs[0].ports, vec![8080, 9090]);
    }

    #[test]
    fn parse_services_skips_malformed_items() {
        // Missing name, missing ports, non-integer port —
        // all silently skipped or yield empty ports list.
        // Defensive against kubectl JSON shape drift.
        let payload = json!({
            "items": [
                { "spec": { "ports": [{ "port": 80 }] } },           // no name
                { "metadata": { "name": "noports" } },                // no spec.ports
                { "metadata": { "name": "stringport" },
                  "spec": { "ports": [{ "port": "eighty" }] } },    // non-int port
                { "metadata": { "name": "ok" },
                  "spec": { "ports": [{ "port": 80 }] } }
            ]
        });
        let svcs = parse_services(&payload);
        let names: Vec<&str> = svcs.iter().map(|s| s.name.as_str()).collect();
        // "noports" + "stringport" present но с empty ports;
        // "no name" entry dropped entirely.
        assert!(names.contains(&"noports"));
        assert!(names.contains(&"stringport"));
        assert!(names.contains(&"ok"));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn parse_services_empty_payload_returns_empty_vec() {
        let payload = json!({ "items": [] });
        assert!(parse_services(&payload).is_empty());
        let payload = json!({});
        assert!(parse_services(&payload).is_empty());
    }

    #[test]
    fn find_apprafter_app_name_picks_application_entry_from_status_resources() {
        // Walk-fix #1 post-B.1.79b regression guard.
        // Real-world `kubectl get application -o json` shape:
        // status.resources lists every resource Argo CD
        // tracks. The first apprafter.io/Application entry
        // is the inner app name we need for the operator's
        // label selector.
        let app = json!({
            "status": {
                "resources": [
                    {
                        "group": "",
                        "kind": "Namespace",
                        "name": "apprafter",
                        "version": "v1"
                    },
                    {
                        "group": "apprafter.io",
                        "kind": "Application",
                        "name": "landing-web",
                        "namespace": "apprafter",
                        "version": "v1alpha1"
                    }
                ]
            }
        });
        assert_eq!(
            find_apprafter_app_name(&app),
            Some("landing-web".to_string())
        );
    }

    #[test]
    fn find_apprafter_app_name_returns_none_for_raw_yaml_apps() {
        // App registered с raw helm / kustomize content
        // (no AppRafter Application CR в the rendered
        // manifests). `app open` errors with а clear
        // diagnostic; this test pins that the helper
        // returns None rather than misidentifying е.g. а
        // Deployment as the inner app name.
        let app = json!({
            "status": {
                "resources": [
                    {
                        "group": "apps",
                        "kind": "Deployment",
                        "name": "my-app",
                        "version": "v1"
                    },
                    {
                        "group": "",
                        "kind": "Service",
                        "name": "my-app",
                        "version": "v1"
                    }
                ]
            }
        });
        assert_eq!(find_apprafter_app_name(&app), None);
    }

    #[test]
    fn find_apprafter_app_name_returns_none_when_status_missing() {
        // Fresh Application CR без а status block yet —
        // Argo CD не ещё reconciled. Helper must not panic.
        let app = json!({ "spec": { "destination": { "namespace": "x" } } });
        assert_eq!(find_apprafter_app_name(&app), None);
    }

    #[test]
    fn find_apprafter_app_name_picks_first_when_multiple_apprafter_applications() {
        // Defensive — if а manifest somehow renders two
        // apprafter.io/Application CRs, the helper picks
        // the first. Walk-fix #2 post-B.1.79b territory if
        // operators actually do this; for now, take the
        // first deterministic-order entry.
        let app = json!({
            "status": {
                "resources": [
                    {
                        "group": "apprafter.io",
                        "kind": "Application",
                        "name": "first",
                        "version": "v1alpha1"
                    },
                    {
                        "group": "apprafter.io",
                        "kind": "Application",
                        "name": "second",
                        "version": "v1alpha1"
                    }
                ]
            }
        });
        assert_eq!(find_apprafter_app_name(&app), Some("first".to_string()));
    }

    #[test]
    fn select_service_happy_single_match() {
        let svc = ServiceInfo {
            name: "landing-web".into(),
            ports: vec![80],
        };
        let selected = select_service(std::slice::from_ref(&svc), "apprafter-landing-web").unwrap();
        assert_eq!(selected, svc);
    }

    #[test]
    fn select_service_zero_matches_errors_with_diagnostic_hint() {
        let err = select_service(&[], "missing-app").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("missing-app"),
            "error must echo the apprafter app name; got: {msg}"
        );
        assert!(
            msg.contains("app.kubernetes.io/name"),
            "error must name the label the operator stamps; got: {msg}"
        );
        assert!(
            msg.contains("kubectl get applications.apprafter.io"),
            "error must hint at how to introspect operator-side state; got: {msg}"
        );
    }

    #[test]
    fn select_service_multi_match_lists_candidate_names() {
        let svcs = vec![
            ServiceInfo {
                name: "first".into(),
                ports: vec![80],
            },
            ServiceInfo {
                name: "second".into(),
                ports: vec![3000],
            },
        ];
        let err = select_service(&svcs, "myapp").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("first"));
        assert!(msg.contains("second"));
    }

    #[test]
    fn resolve_container_port_prefers_override() {
        // `--container-port 9090` wins over Service's port 80
        // — operator's explicit pin.
        let svc = ServiceInfo {
            name: "api".into(),
            ports: vec![80, 443],
        };
        assert_eq!(resolve_container_port(Some(9090), &svc).unwrap(), 9090);
    }

    #[test]
    fn resolve_container_port_falls_back_to_first_service_port() {
        let svc = ServiceInfo {
            name: "api".into(),
            ports: vec![8080, 9090],
        };
        assert_eq!(resolve_container_port(None, &svc).unwrap(), 8080);
    }

    #[test]
    fn resolve_container_port_errors_when_service_has_no_ports_and_no_override() {
        // Walk regression: AppRafter operator skips the
        // Service entirely when `spec.expose` is omitted —
        // discovered Service might exist (e.g. from а
        // sidecar) but declare zero ports for the user app's
        // traffic. Don't silently pick а random port.
        let svc = ServiceInfo {
            name: "headless".into(),
            ports: vec![],
        };
        let err = resolve_container_port(None, &svc).unwrap_err();
        assert!(err.to_string().contains("--container-port"));
    }

    #[test]
    fn is_port_free_returns_false_when_port_is_bound() {
        // Bind а listener ourselves, hold it, ask the prober
        // — must report false. Then drop and ask again —
        // must report true. Race window between drop and
        // re-probe is microseconds; safe enough for this
        // test.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral");
        let port = listener.local_addr().unwrap().port();
        assert!(!is_port_free(port), "bound port must report not free");
        drop(listener);
        // After drop, the kernel may take а moment к release;
        // re-probing immediately would test kernel timing,
        // not our logic. Leaving the second assertion out —
        // the "bound port reports busy" check is what guards
        // against а false-positive `pick_local_port` returning
        // а port that kubectl can't actually bind.
    }

    #[test]
    fn pick_local_port_returns_starting_port_when_free() {
        // Bind а listener on an ephemeral port to keep the
        // probe deterministic, then call pick_local_port
        // starting from а different ephemeral port. We can't
        // hard-code 8080 — CI runners may have it bound.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral");
        let busy_port = listener.local_addr().unwrap().port();

        // Start from busy_port — first probe should land on
        // busy_port+1 (assuming +1 is free, which is virtually
        // always the case for а freshly-allocated ephemeral).
        let picked = pick_local_port(busy_port).expect("must find а free port");
        assert_ne!(picked, busy_port, "must skip the bound port");
        assert!(
            picked >= busy_port && picked < busy_port + LOCAL_PORT_MAX_PROBES,
            "picked port {picked} must be within probe window [{busy_port}, {})",
            busy_port + LOCAL_PORT_MAX_PROBES
        );
    }
}
