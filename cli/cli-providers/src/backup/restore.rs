// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d restore: ordered step-decision state machine (pure, unit-testable).

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreMode {
    /// Restore into an already-running, bootstrapped target (modes a-into-running / b).
    IntoRunning,
    /// Re-provision a fresh cluster in the current target first (mode a).
    Reprovision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreStep {
    Reprovision,
    RestoreArtifact,
    ApplyPlatformStack,
    ApplySourceCredentials,
    /// Apply Argo Apps with `syncPolicy.automated` stripped + AppRafter
    /// `Application` CRs with replicas=0 — claims provision, NO workload pod (H2).
    ApplyAppsGated,
    WaitClaimsBound,
    LoadData,
    ReSealUserSecrets,
    /// Patch Application replicas back to the backed-up values + re-enable Argo
    /// auto-sync — workloads come up on already-loaded data (H2).
    ResumeWorkloads,
    /// `--data-only`: scale the existing app's workload to 0 (+ disable its Argo
    /// auto-sync) so the load doesn't race a running pod.
    SuspendWorkloads,
}

/// Decide the ordered restore steps for a mode + `--data-only`.
pub fn restore_steps(mode: RestoreMode, data_only: bool) -> Vec<RestoreStep> {
    use RestoreStep::*;
    if data_only {
        // recover a volume/DB into an existing cluster: suspend the running
        // workload, load, resume — no CR/secret replay (H2 race avoidance).
        return vec![RestoreArtifact, SuspendWorkloads, LoadData, ResumeWorkloads];
    }
    let mut steps = Vec::new();
    if mode == RestoreMode::Reprovision {
        steps.push(Reprovision);
    }
    steps.extend([
        RestoreArtifact,
        ApplyPlatformStack,
        ApplySourceCredentials,
        ApplyAppsGated,
        WaitClaimsBound,
        LoadData,
        ReSealUserSecrets,
        ResumeWorkloads,
    ]);
    steps
}

/// Gate an AppRafter `Application` CR so its claims provision but NO workload
/// pod comes up: set `spec.base.replicas = 0` and every
/// `spec.environments.<env>.replicas = 0` (the latter is set EVEN WHEN the env
/// did not previously carry a `replicas` field, so an env that inherited a
/// non-zero base replica count can't sneak a pod up before `LoadData`).
///
/// This is the load-bearing H2 transform of the restore-into-running flow: the
/// app's ResourceClaims must regenerate (so the fresh connection Secret + PVCs
/// exist for `LoadData`), but the workload must stay down until the data is
/// loaded — `ResumeWorkloads` then patches the recorded replica counts back.
///
/// Returns a fresh `Value`; the input is not mutated. Pure — the unit-tested
/// seam of the gated apply.
pub fn zero_replicas(app_cr: &Value) -> Value {
    let mut out = app_cr.clone();

    // spec.base.replicas = 0 (create base if the CR somehow lacks it; an
    // AppRafter Application always has spec.base, but be defensive).
    {
        let spec = ensure_object(&mut out, "spec");
        let base = ensure_child_object(spec, "base");
        base.insert("replicas".to_string(), Value::from(0));
    }

    // spec.environments.<env>.replicas = 0 for EVERY env key — set even when
    // the env had no replicas field, so an inherited base count can't leak a
    // pod up. Only touch envs that are objects (a non-object env value is
    // schema-invalid and left untouched for the apply to surface).
    if let Some(envs) = out
        .pointer_mut("/spec/environments")
        .and_then(Value::as_object_mut)
    {
        for (_name, env) in envs.iter_mut() {
            if let Some(env_obj) = env.as_object_mut() {
                env_obj.insert("replicas".to_string(), Value::from(0));
            }
        }
    }

    out
}

/// Borrow (creating if absent) the named child object of a JSON object value.
fn ensure_object<'a>(v: &'a mut Value, key: &str) -> &'a mut serde_json::Map<String, Value> {
    if !v.is_object() {
        *v = Value::Object(serde_json::Map::new());
    }
    let obj = v.as_object_mut().expect("just ensured object");
    ensure_child_object(obj, key)
}

/// Borrow (creating if absent) the named child object of a JSON map.
fn ensure_child_object<'a>(
    obj: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, Value> {
    obj.entry(key.to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    obj.get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("entry just inserted as object")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_replicas_gates_base_and_every_env() {
        let app = serde_json::json!({"spec":{"base":{"image":"x","replicas":3},
            "environments":{"dev":{"replicas":2},"prod":{"image":"y"}}}});
        let z = zero_replicas(&app);
        assert_eq!(z["spec"]["base"]["replicas"], 0);
        assert_eq!(z["spec"]["environments"]["dev"]["replicas"], 0);
        assert_eq!(z["spec"]["environments"]["prod"]["replicas"], 0); // set even if absent
                                                                      // Non-replica fields are preserved untouched.
        assert_eq!(z["spec"]["base"]["image"], "x");
        assert_eq!(z["spec"]["environments"]["prod"]["image"], "y");
    }

    #[test]
    fn zero_replicas_handles_app_with_no_environments() {
        let app = serde_json::json!({"spec":{"base":{"image":"x","replicas":5}}});
        let z = zero_replicas(&app);
        assert_eq!(z["spec"]["base"]["replicas"], 0);
        // No environments key invented.
        assert!(z["spec"].get("environments").is_none());
    }

    #[test]
    fn full_restore_into_running_target_gates_workloads_until_after_load() {
        let steps = restore_steps(RestoreMode::IntoRunning, false);
        assert!(!steps.contains(&RestoreStep::Reprovision));
        assert_eq!(steps.first(), Some(&RestoreStep::RestoreArtifact));
        let i = |s| steps.iter().position(|x| *x == s).unwrap();
        assert!(i(RestoreStep::ApplyPlatformStack) < i(RestoreStep::ApplySourceCredentials));
        assert!(i(RestoreStep::ApplySourceCredentials) < i(RestoreStep::ApplyAppsGated));
        assert!(i(RestoreStep::ApplyAppsGated) < i(RestoreStep::WaitClaimsBound));
        assert!(i(RestoreStep::WaitClaimsBound) < i(RestoreStep::LoadData));
        assert!(i(RestoreStep::LoadData) < i(RestoreStep::ReSealUserSecrets));
        assert_eq!(steps.last(), Some(&RestoreStep::ResumeWorkloads));
        assert!(i(RestoreStep::LoadData) < i(RestoreStep::ResumeWorkloads));
    }

    #[test]
    fn reprovision_mode_prepends_reprovision() {
        let steps = restore_steps(RestoreMode::Reprovision, false);
        assert_eq!(steps.first(), Some(&RestoreStep::Reprovision));
        assert_eq!(steps.last(), Some(&RestoreStep::ResumeWorkloads));
    }

    #[test]
    fn data_only_suspends_loads_resumes() {
        let steps = restore_steps(RestoreMode::IntoRunning, true);
        assert_eq!(
            steps,
            vec![
                RestoreStep::RestoreArtifact,
                RestoreStep::SuspendWorkloads,
                RestoreStep::LoadData,
                RestoreStep::ResumeWorkloads,
            ]
        );
    }
}
