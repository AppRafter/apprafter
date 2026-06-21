// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d restore: ordered step-decision state machine (pure, unit-testable).

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

#[cfg(test)]
mod tests {
    use super::*;

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
