// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The `PlatformStack` the operator keeps (`apprafter-system/default`) and the backup runner's
//! status ConfigMap, as `kubectl get … -o json` returns them.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::time::Ago;

/// Where the stack lives.
pub const NAMESPACE: &str = "apprafter-system";
/// The stack's name.
pub const NAME: &str = "default";
/// The platform release a healthy stack runs. The fixtures' platform versions are `1.x` (and
/// [`BEFORE_REPORTING`]), outside the CLI's own 0.2 series: the goldens mask the CLI's own
/// version as `<VERSION>` wherever it stands alone, and would mask an equal value too (a CLI
/// that reaches 1.3.7 … 1.5.0 one day moves these goldens; the D.5 hand-off names it).
pub const CURRENT: &str = "1.4.2";
/// A newer release on the channel.
pub const NEWER: &str = "1.5.0";
/// The release before [`CURRENT`].
pub const OLDER: &str = "1.3.7";
/// A release older than the first operator that reports on backups (0.2.80) and on stalled
/// reconciles (0.2.81): such conditions on its stack were left by a newer one.
pub const BEFORE_REPORTING: &str = "0.2.9";

/// A condition as the operator writes one.
pub fn condition(type_: &str, status: &str, reason: &str, message: &str, since: &str) -> Value {
    json!({
        "type": type_,
        "status": status,
        "reason": reason,
        "message": message,
        "lastTransitionTime": since
    })
}

/// A `PlatformStack`, built from a healthy one by the methods below; every time is an offset
/// from the `now` it was made with.
#[derive(Debug, Clone)]
pub struct Stack {
    now: DateTime<Utc>,
    json: Value,
}

impl Stack {
    /// A healthy tier-1 stack at [`CURRENT`]: channel `stable`, unpinned, no automatic
    /// upgrade, backups off, `Ready` and `Synced` True, no upgrade waiting, the upstream
    /// checked 3 hours ago, two releases in its history.
    pub fn healthy(now: DateTime<Utc>) -> Self {
        let json = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "PlatformStack",
            "metadata": { "name": NAME, "namespace": NAMESPACE },
            "spec": {
                "channel": "stable",
                "autoUpgrade": false,
                "values": { "tier": 1 },
                "backup": { "enabled": false }
            },
            "status": {
                "currentVersion": CURRENT,
                "targetVersion": CURRENT,
                "availableVersion": CURRENT,
                "lastUpstreamCheck": Ago::Hours(3).before(now),
                "conditions": [
                    condition("Ready", "True", "Healthy", "the platform Application is Healthy", &Ago::Days(3).before(now)),
                    condition("Synced", "True", "Synced", "the platform Application is Synced", &Ago::Days(3).before(now)),
                    condition("UpgradeAvailable", "False", "UpToDate", "no newer release on channel stable", &Ago::Hours(3).before(now))
                ],
                "versionHistory": [
                    { "version": CURRENT, "appliedAt": Ago::Days(3).before(now), "outcome": "succeeded" },
                    { "version": OLDER, "appliedAt": Ago::Days(12).before(now), "outcome": "succeeded" }
                ]
            }
        });
        Stack { now, json }
    }

    /// [`Stack::healthy`] before the operator wrote any status.
    pub fn without_status(now: DateTime<Utc>) -> Self {
        let mut stack = Self::healthy(now);
        if let Some(object) = stack.json.as_object_mut() {
            object.remove("status");
        }
        stack
    }

    /// Runs `version` (current and target).
    pub fn current(mut self, version: &str) -> Self {
        self.json["status"]["currentVersion"] = json!(version);
        self.json["status"]["targetVersion"] = json!(version);
        self
    }

    /// The channel's newest release the resolver may pick.
    pub fn available(mut self, version: &str) -> Self {
        self.json["status"]["availableVersion"] = json!(version);
        self
    }

    /// The condition `type_`, so since `since`; one already of that type is replaced.
    pub fn condition(
        mut self,
        type_: &str,
        status: &str,
        reason: &str,
        message: &str,
        since: Ago,
    ) -> Self {
        let new = condition(type_, status, reason, message, &since.before(self.now));
        let slot = &mut self.json["status"]["conditions"];
        if !slot.is_array() {
            *slot = json!([]);
        }
        let list = slot.as_array_mut().expect("conditions are a list");
        match list.iter_mut().find(|c| c["type"] == type_) {
            Some(old) => *old = new,
            None => list.push(new),
        }
        self
    }

    /// Backups on (`spec.backup.enabled`).
    pub fn backups_on(mut self) -> Self {
        self.json["spec"]["backup"]["enabled"] = json!(true);
        self
    }

    /// `apprafter backup prune` last ran against this cluster `ago` (the
    /// `apprafter.io/last-prune` stamp).
    pub fn last_prune(mut self, ago: Ago) -> Self {
        let at = ago.before(self.now);
        self.json["metadata"]["annotations"]["apprafter.io/last-prune"] = json!(at);
        self
    }

    /// The cluster default for VPA autoscaling (`spec.resources.autoscale.mode`).
    pub fn autoscale(mut self, mode: &str) -> Self {
        self.json["spec"]["resources"]["autoscale"]["mode"] = json!(mode);
        self
    }

    /// The cluster's default environment (`spec.defaultEnvironment`).
    pub fn default_environment(mut self, environment: &str) -> Self {
        self.json["spec"]["defaultEnvironment"] = json!(environment);
        self
    }

    /// When the operator last asked the upstream: any time, one ahead of the clock included
    /// (B76's re-check answer).
    pub fn last_upstream_check(mut self, at: String) -> Self {
        self.json["status"]["lastUpstreamCheck"] = json!(at);
        self
    }

    /// The version history: `(version, applied, outcome)`, `None` for an entry with no
    /// `appliedAt`.
    pub fn history(mut self, entries: &[(&str, Option<Ago>, &str)]) -> Self {
        let now = self.now;
        let list: Vec<Value> = entries
            .iter()
            .map(|(version, applied, outcome)| {
                let mut entry = json!({ "version": version, "outcome": outcome });
                if let Some(ago) = applied {
                    entry["appliedAt"] = json!(ago.before(now));
                }
                entry
            })
            .collect();
        self.json["status"]["versionHistory"] = Value::Array(list);
        self
    }

    /// Component versions the operator reports (`status.componentVersions`) and components
    /// frozen at a pin (`spec.overrides.<c>.pin`): what d5-c's Components section reads (B13)
    /// and today's CLI does not print.
    pub fn components(mut self, versions: &[(&str, &str)], frozen: &[(&str, &str)]) -> Self {
        for (name, version) in versions {
            self.json["status"]["componentVersions"][*name] = json!(version);
        }
        for (name, pin) in frozen {
            self.json["spec"]["overrides"][*name]["pin"] = json!(pin);
        }
        self
    }

    /// The object, as `kubectl get platformstack default -n apprafter-system -o json` prints
    /// it.
    pub fn json(self) -> Value {
        self.json
    }
}

/// The backup runner's own record (`apprafter-backup-status`): its last success `ago`.
pub fn backup_status(now: DateTime<Utc>, last_success: Ago) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": { "name": "apprafter-backup-status", "namespace": NAMESPACE },
        "data": { "lastSuccess": last_success.before(now) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-16T12:00:00Z")
            .expect("a test time")
            .with_timezone(&Utc)
    }

    #[test]
    fn a_condition_replaces_one_of_its_type() {
        let stack = Stack::healthy(now())
            .condition("Synced", "False", "OutOfSync", "drifted", Ago::Hours(3))
            .json();
        let synced: Vec<&Value> = stack["status"]["conditions"]
            .as_array()
            .expect("conditions")
            .iter()
            .filter(|c| c["type"] == "Synced")
            .collect();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0]["status"], "False");
        assert_eq!(synced[0]["lastTransitionTime"], "2026-10-16T09:00:00Z");
    }

    #[test]
    fn a_healthy_stack_runs_the_current_release_with_backups_off() {
        let stack = Stack::healthy(now()).json();
        assert_eq!(stack["status"]["currentVersion"], CURRENT);
        assert_eq!(stack["status"]["availableVersion"], CURRENT);
        assert_eq!(stack["status"]["lastUpstreamCheck"], "2026-10-16T09:00:00Z");
        assert_eq!(stack["spec"]["backup"]["enabled"], false);
        let stack = Stack::healthy(now())
            .backups_on()
            .autoscale("up-only")
            .default_environment("prod")
            .json();
        assert_eq!(stack["spec"]["backup"]["enabled"], true);
        assert_eq!(stack["spec"]["resources"]["autoscale"]["mode"], "up-only");
        assert_eq!(stack["spec"]["defaultEnvironment"], "prod");
    }

    #[test]
    fn a_stack_without_status_has_none_and_takes_a_condition() {
        assert!(Stack::without_status(now()).json().get("status").is_none());
        let stack = Stack::without_status(now())
            .condition("Ready", "True", "Healthy", "", Ago::Hours(3))
            .json();
        assert_eq!(
            stack["status"]["conditions"].as_array().map(Vec::len),
            Some(1)
        );
    }
}
