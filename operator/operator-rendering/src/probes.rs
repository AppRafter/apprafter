// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure probe resolution for 2.28 (ADR 0065 §1).
//!
//! Turns an `Application`'s effective `probes` block plus its effective
//! `expose.port` into the three `k8s_openapi` probes a container carries,
//! applying the platform's timing defaults, the default readiness probe and
//! the derived startup probe.
//!
//! **Every default lives here rather than in the CUE or the CRD**, and that
//! is this repository's standing rule rather than a choice made for probes:
//! `crdgen`'s `structural::resolve` strips CUE `*x` defaults and the R4-M2
//! assertion (`operator/crdgen/src/check.rs`) fails the build if any
//! `default:` survives into a rendered CRD, on the grounds that behaviour
//! belongs to the renderer and not to the apiserver (ADR 0047). The rule
//! earns its keep here: a CRD default is stamped into the stored object at
//! admission, so revising `DEFAULT_PERIOD_SECONDS` later would leave every
//! existing Application carrying the old number invisibly.
//!
//! The consequence to remember is that neither the manifest nor the stored
//! object shows the effective numbers — `apprafter app status` is where a
//! reader sees them.

use k8s_openapi::api::core::v1::{HTTPGetAction, HTTPHeader, Probe as K8sProbe, TCPSocketAction};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use operator_core::{Probe, Probes};

/// Timing defaults for a DECLARED probe that omitted them.
///
/// All Kubernetes' own defaults except `timeoutSeconds`, which is 2 rather
/// than 1: on Tier 1 the node has one or two shared vCPUs, and a one-second
/// budget for an in-pod HTTP round trip turns an ordinary GC pause into a
/// probe failure. Recorded as a CHOICE, not a finding — ADR 0065 §5(d) owes
/// the measurement, and if a 1s timeout shows no false failures under load
/// this goes back to Kubernetes' number.
const DEFAULT_INITIAL_DELAY_SECONDS: i32 = 0;
const DEFAULT_PERIOD_SECONDS: i32 = 10;
const DEFAULT_TIMEOUT_SECONDS: i32 = 2;
const DEFAULT_FAILURE_THRESHOLD: i32 = 3;
const DEFAULT_SUCCESS_THRESHOLD: i32 = 1;

/// The derived startup probe's cadence: check every 5s, allow 60 failures —
/// five minutes to come up before liveness takes over.
///
/// Deliberately NOT inherited from the liveness probe's own period. A
/// liveness period is tuned for how fast a HANG should be caught, which is
/// the opposite question from how long a START should be tolerated, so
/// inheriting it would make a tight liveness probe (the responsible choice)
/// silently shorten the startup budget (the dangerous one).
const DERIVED_STARTUP_PERIOD_SECONDS: i32 = 5;
const DERIVED_STARTUP_FAILURE_THRESHOLD: i32 = 60;

/// Which probe slot is being rendered. Only Kubernetes' `successThreshold`
/// rule distinguishes them at render time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeKind {
    Liveness,
    Readiness,
    Startup,
}

/// The three rendered probes for one container.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderedProbes {
    pub liveness: Option<K8sProbe>,
    pub readiness: Option<K8sProbe>,
    pub startup: Option<K8sProbe>,
}

/// Translate one declared `Probe` into the Kubernetes shape.
///
/// Returns `None` when the probe is disabled, or when no port can be
/// resolved. The latter is a defensive path only — `validate_probes` in the
/// admission webhook rejects a probe with neither its own `port` nor an
/// effective `expose.port`, naming which probe. Emitting a port-0 probe
/// instead would produce a pod that can never become Ready for a reason
/// nothing names, which is strictly worse than rendering no probe at all.
pub fn to_k8s_probe(p: &Probe, expose_port: Option<i32>, kind: ProbeKind) -> Option<K8sProbe> {
    if !p.enabled.unwrap_or(true) {
        return None;
    }
    let port = p.port.or(expose_port)?;

    // The FORM is discriminated by `path`, not by a nested action object
    // (ADR 0065 §1.1): present => HTTP GET, absent => TCP connect.
    let (http_get, tcp_socket) = match p.path.as_deref() {
        Some(path) => (
            Some(HTTPGetAction {
                path: Some(path.to_string()),
                port: IntOrString::Int(port),
                // The manifest enum is lowercase so it reads like the rest of
                // the platform; Kubernetes only accepts HTTP/HTTPS.
                scheme: Some(p.scheme.as_deref().unwrap_or("http").to_ascii_uppercase()),
                http_headers: p.headers.as_ref().map(|h| {
                    // BTreeMap iteration is lexicographic, so the rendered
                    // header list is byte-stable across reconciles and the
                    // server-side apply stays a no-op.
                    h.iter()
                        .map(|(name, value)| HTTPHeader {
                            name: name.clone(),
                            value: value.clone(),
                        })
                        .collect()
                }),
                ..Default::default()
            }),
            None,
        ),
        None => (
            None,
            Some(TCPSocketAction {
                port: IntOrString::Int(port),
                ..Default::default()
            }),
        ),
    };

    // Kubernetes requires successThreshold == 1 on liveness and startup and
    // rejects the Deployment otherwise. The webhook already refuses any other
    // value at the Application, where the field name is visible; this clamp
    // is what keeps an object that slipped past it (an older webhook, a
    // direct CR write) from turning into an apiserver rejection far from its
    // cause.
    let success_threshold = match kind {
        ProbeKind::Liveness | ProbeKind::Startup => 1,
        ProbeKind::Readiness => p.success_threshold.unwrap_or(DEFAULT_SUCCESS_THRESHOLD),
    };

    Some(K8sProbe {
        http_get,
        tcp_socket,
        initial_delay_seconds: Some(
            p.initial_delay_seconds
                .unwrap_or(DEFAULT_INITIAL_DELAY_SECONDS),
        ),
        period_seconds: Some(p.period_seconds.unwrap_or(DEFAULT_PERIOD_SECONDS)),
        timeout_seconds: Some(p.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS)),
        failure_threshold: Some(p.failure_threshold.unwrap_or(DEFAULT_FAILURE_THRESHOLD)),
        success_threshold: Some(success_threshold),
        ..Default::default()
    })
}

/// Resolve the effective `probes` block plus the effective `expose.port`
/// into the three rendered probes, applying the two platform behaviours:
///
///  - **Default readiness** (ADR 0065 §1.3). No declared `readiness` and a
///    declared port => a TCP connect on that port. Nothing is guessed: the
///    port is the manifest's own, and a TCP connect asserts only what the
///    Service already assumes. Without it every rolling update has a window
///    in which the new pod is Ready and taking traffic while its process is
///    still binding.
///  - **Derived startup** (§1.4). A declared, enabled `liveness` with no
///    `startup` => a startup probe against the same target with a long
///    failure budget. A derived startup probe cannot make a working
///    application fail; it can only postpone the first liveness kill, which
///    is what stops "I added one liveness probe" from CrashLooping a
///    forty-second boot forever.
pub fn resolve_probes(declared: Option<&Probes>, expose_port: Option<i32>) -> RenderedProbes {
    let liveness = declared
        .and_then(|d| d.liveness.as_ref())
        .and_then(|p| to_k8s_probe(p, expose_port, ProbeKind::Liveness));

    let readiness = match declared.and_then(|d| d.readiness.as_ref()) {
        Some(p) => to_k8s_probe(p, expose_port, ProbeKind::Readiness),
        None => expose_port.and_then(|port| {
            to_k8s_probe(
                &Probe {
                    port: Some(port),
                    ..Default::default()
                },
                None,
                ProbeKind::Readiness,
            )
        }),
    };

    let startup = match declared.and_then(|d| d.startup.as_ref()) {
        // An explicit startup probe wins outright — no field-level merge with
        // liveness, which would produce a probe neither side wrote.
        Some(p) => to_k8s_probe(p, expose_port, ProbeKind::Startup),
        None => declared
            .and_then(|d| d.liveness.as_ref())
            // Derive only from a liveness probe that will actually render;
            // a disabled one protects nothing, so there is nothing to bridge.
            .filter(|l| l.enabled.unwrap_or(true))
            .and_then(|l| {
                to_k8s_probe(
                    &Probe {
                        path: l.path.clone(),
                        port: l.port,
                        scheme: l.scheme.clone(),
                        headers: l.headers.clone(),
                        period_seconds: Some(DERIVED_STARTUP_PERIOD_SECONDS),
                        failure_threshold: Some(DERIVED_STARTUP_FAILURE_THRESHOLD),
                        ..Default::default()
                    },
                    expose_port,
                    ProbeKind::Startup,
                )
            }),
    };

    RenderedProbes {
        liveness,
        readiness,
        startup,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{Probe, Probes};

    #[test]
    fn a_declared_http_probe_takes_the_defaults_it_omitted() {
        let p = Probe {
            path: Some("/healthz".into()),
            ..Default::default()
        };
        let out = to_k8s_probe(&p, Some(8080), ProbeKind::Readiness).expect("probe");
        let http = out.http_get.expect("httpGet");
        assert_eq!(http.path.as_deref(), Some("/healthz"));
        assert_eq!(
            http.port,
            IntOrString::Int(8080),
            "port falls back to expose.port"
        );
        assert_eq!(http.scheme.as_deref(), Some("HTTP"));
        assert!(out.tcp_socket.is_none());
        assert_eq!(out.period_seconds, Some(10));
        assert_eq!(out.timeout_seconds, Some(2));
        assert_eq!(out.failure_threshold, Some(3));
        assert_eq!(out.initial_delay_seconds, Some(0));
    }

    #[test]
    fn a_pathless_probe_is_a_tcp_connect() {
        let p = Probe {
            port: Some(5432),
            ..Default::default()
        };
        let out = to_k8s_probe(&p, None, ProbeKind::Readiness).expect("probe");
        assert_eq!(
            out.tcp_socket.expect("tcpSocket").port,
            IntOrString::Int(5432)
        );
        assert!(out.http_get.is_none());
    }

    #[test]
    fn an_https_scheme_reaches_kubernetes_uppercased() {
        // The manifest enum is lowercase to read like the rest of the
        // platform; Kubernetes only accepts HTTP/HTTPS.
        let p = Probe {
            path: Some("/healthz".into()),
            scheme: Some("https".into()),
            ..Default::default()
        };
        let out = to_k8s_probe(&p, Some(8443), ProbeKind::Readiness).expect("probe");
        assert_eq!(
            out.http_get.expect("httpGet").scheme.as_deref(),
            Some("HTTPS")
        );
    }

    #[test]
    fn headers_reach_the_http_action() {
        let p = Probe {
            path: Some("/healthz".into()),
            headers: Some(std::collections::BTreeMap::from([(
                "X-Probe".to_string(),
                "apprafter".to_string(),
            )])),
            ..Default::default()
        };
        let hs = to_k8s_probe(&p, Some(8080), ProbeKind::Readiness)
            .expect("probe")
            .http_get
            .expect("httpGet")
            .http_headers
            .expect("headers");
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].name, "X-Probe");
        assert_eq!(hs[0].value, "apprafter");
    }

    #[test]
    fn a_disabled_probe_renders_nothing() {
        let p = Probe {
            enabled: Some(false),
            path: Some("/healthz".into()),
            ..Default::default()
        };
        assert!(to_k8s_probe(&p, Some(8080), ProbeKind::Readiness).is_none());
    }

    #[test]
    fn a_probe_with_no_port_anywhere_renders_nothing() {
        // The webhook rejects this at admission; the renderer must not panic
        // or emit a port-0 probe on the defensive path — a pod that can never
        // become Ready for a reason nothing names is the worse outcome.
        let p = Probe {
            path: Some("/healthz".into()),
            ..Default::default()
        };
        assert!(to_k8s_probe(&p, None, ProbeKind::Readiness).is_none());
    }

    #[test]
    fn liveness_and_startup_never_carry_a_success_threshold_above_one() {
        let p = Probe {
            path: Some("/livez".into()),
            success_threshold: Some(3),
            ..Default::default()
        };
        for kind in [ProbeKind::Liveness, ProbeKind::Startup] {
            let out = to_k8s_probe(&p, Some(8080), kind).expect("probe");
            assert_eq!(out.success_threshold, Some(1), "{kind:?}");
        }
        let out = to_k8s_probe(&p, Some(8080), ProbeKind::Readiness).expect("probe");
        assert_eq!(
            out.success_threshold,
            Some(3),
            "readiness keeps the declared value"
        );
    }

    #[test]
    fn an_undeclared_readiness_becomes_a_tcp_probe_on_the_exposed_port() {
        let out = resolve_probes(None, Some(8080));
        let r = out.readiness.expect("default readiness");
        assert_eq!(
            r.tcp_socket.expect("tcpSocket").port,
            IntOrString::Int(8080)
        );
        assert!(out.liveness.is_none(), "no liveness is invented");
        assert!(out.startup.is_none());
    }

    #[test]
    fn an_undeclared_readiness_on_an_unexposed_app_stays_absent() {
        let out = resolve_probes(None, None);
        assert!(out.readiness.is_none());
        assert!(out.liveness.is_none());
        assert!(out.startup.is_none());
    }

    #[test]
    fn readiness_enabled_false_suppresses_the_default() {
        let declared = Probes {
            readiness: Some(Probe {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(resolve_probes(Some(&declared), Some(8080))
            .readiness
            .is_none());
    }

    #[test]
    fn a_declared_liveness_derives_a_startup_probe_against_the_same_target() {
        let declared = Probes {
            liveness: Some(Probe {
                path: Some("/livez".into()),
                scheme: Some("https".into()),
                period_seconds: Some(30),
                ..Default::default()
            }),
            ..Default::default()
        };
        let out = resolve_probes(Some(&declared), Some(8080));
        let s = out.startup.expect("derived startup");
        let http = s.http_get.expect("httpGet");
        assert_eq!(
            http.path.as_deref(),
            Some("/livez"),
            "same target as liveness"
        );
        assert_eq!(http.scheme.as_deref(), Some("HTTPS"));
        assert_eq!(
            s.period_seconds,
            Some(5),
            "NOT liveness's 30 — a liveness period answers how fast a hang is \
             caught, which is the opposite question from how long a start is tolerated"
        );
        assert_eq!(
            s.failure_threshold,
            Some(60),
            "five minutes of startup budget"
        );
    }

    #[test]
    fn an_explicit_startup_wins_outright_over_the_derivation() {
        let declared = Probes {
            liveness: Some(Probe {
                path: Some("/livez".into()),
                ..Default::default()
            }),
            startup: Some(Probe {
                path: Some("/started".into()),
                failure_threshold: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        };
        let s = resolve_probes(Some(&declared), Some(8080))
            .startup
            .expect("startup");
        assert_eq!(s.http_get.unwrap().path.as_deref(), Some("/started"));
        assert_eq!(
            s.failure_threshold,
            Some(10),
            "no field-level merge with liveness"
        );
    }

    #[test]
    fn a_disabled_liveness_derives_no_startup_probe() {
        let declared = Probes {
            liveness: Some(Probe {
                enabled: Some(false),
                path: Some("/livez".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(resolve_probes(Some(&declared), Some(8080))
            .startup
            .is_none());
    }

    #[test]
    fn a_declared_liveness_on_an_unexposed_app_derives_nothing_and_renders_nothing() {
        // No port anywhere: the webhook rejects this at admission, and the
        // renderer must produce neither a liveness nor a derived startup.
        let declared = Probes {
            liveness: Some(Probe {
                path: Some("/livez".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let out = resolve_probes(Some(&declared), None);
        assert!(out.liveness.is_none());
        assert!(out.startup.is_none());
    }
}
