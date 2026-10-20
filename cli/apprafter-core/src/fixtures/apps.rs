// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! AppRafter `Application`s (`application.apprafter.io`) and the Argo CD `Application`s that
//! register them, as the roll-up of `apprafter status` lists them.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::time::Ago;

/// An AppRafter `Application`, built from a reconciled one.
#[derive(Debug, Clone)]
pub struct App {
    now: DateTime<Utc>,
    json: Value,
}

impl App {
    /// `namespace/name`, reconciled 3 days ago (`Ready` True), nothing on its problem ledger.
    pub fn ready(now: DateTime<Utc>, namespace: &str, name: &str) -> Self {
        let json = json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": "Application",
            "metadata": { "name": name, "namespace": namespace },
            "status": {
                "conditions": [{
                    "type": "Ready",
                    "status": "True",
                    "reason": "Reconciled",
                    "message": "",
                    "lastTransitionTime": Ago::Days(3).before(now)
                }],
                "recentProblems": []
            }
        });
        App { now, json }
    }

    /// A problem on its ledger (`status.recentProblems`), first and last seen `last_seen`
    /// ago, `count` sightings.
    pub fn problem(mut self, reason: &str, message: &str, last_seen: Ago, count: u32) -> Self {
        let at = last_seen.before(self.now);
        let entry = json!({
            "reason": reason,
            "message": message,
            "firstSeen": at,
            "lastSeen": at,
            "count": count
        });
        if let Some(list) = self.json["status"]["recentProblems"].as_array_mut() {
            list.push(entry);
        }
        self
    }

    /// The operator holds its reconcile: `Ready` False since `since`, with `reason` and
    /// `message`.
    pub fn held(mut self, reason: &str, message: &str, since: Ago) -> Self {
        let ready = json!({
            "type": "Ready",
            "status": "False",
            "reason": reason,
            "message": message,
            "lastTransitionTime": since.before(self.now)
        });
        self.json["status"]["conditions"] = json!([ready]);
        self
    }

    /// Held at a digest (ADR 0059): the operator honoured the pin to `reference`.
    pub fn pinned(mut self, reference: &str) -> Self {
        self.json["status"]["image"]["pinned"]["resolved"] = json!(reference);
        self
    }

    /// On its way out (`metadata.deletionTimestamp`, a minute ago).
    pub fn deleting(mut self) -> Self {
        let at = Ago::Minutes(1).before(self.now);
        self.json["metadata"]["deletionTimestamp"] = json!(at);
        self
    }

    /// The object, as the cluster-wide list carries it.
    pub fn json(self) -> Value {
        self.json
    }
}

/// The Argo CD `Application` registering `logical` (in `environment`, when given), whose
/// workloads are the AppRafter `Application`s `(namespace, name)`, as its
/// `status.resources` records them.
pub fn registration(logical: &str, environment: Option<&str>, workloads: &[(&str, &str)]) -> Value {
    let name = match environment {
        Some(env) => format!("{logical}-{env}"),
        None => logical.to_string(),
    };
    let mut labels = json!({ "apprafter.io/application": logical });
    if let Some(env) = environment {
        labels["apprafter.io/environment"] = json!(env);
    }
    let resources: Vec<Value> = workloads
        .iter()
        .map(|(ns, cr)| {
            json!({
                "group": "apprafter.io",
                "version": "v1alpha1",
                "kind": "Application",
                "namespace": ns,
                "name": cr,
                "status": "Synced"
            })
        })
        .collect();
    let destination = workloads.first().map(|(ns, _)| *ns).unwrap_or("default");
    json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Application",
        "metadata": { "name": name, "namespace": "argocd", "labels": labels },
        "spec": { "destination": { "namespace": destination } },
        "status": { "resources": resources }
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
    fn a_problem_sits_on_the_ledger_and_a_hold_flips_ready() {
        let app = App::ready(now(), "shop", "shop-prod")
            .problem("OOMKilled", "killed", Ago::Minutes(3), 4)
            .held(
                "EnvSecretMissing",
                "secret shop/db not found",
                Ago::Hours(3),
            )
            .json();
        let problem = &app["status"]["recentProblems"][0];
        assert_eq!(problem["lastSeen"], "2026-10-16T11:57:00Z");
        assert_eq!(problem["count"], 4);
        let ready: Vec<&Value> = app["status"]["conditions"]
            .as_array()
            .expect("conditions")
            .iter()
            .filter(|c| c["type"] == "Ready")
            .collect();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0]["status"], "False");
        assert_eq!(ready[0]["reason"], "EnvSecretMissing");
    }

    #[test]
    fn a_registration_names_each_workload_as_argo_cd_records_it() {
        let reg = registration(
            "shop",
            Some("prod"),
            &[("shop", "shop-prod"), ("shop", "worker")],
        );
        assert_eq!(
            reg["metadata"]["labels"]["apprafter.io/application"],
            "shop"
        );
        assert_eq!(
            reg["metadata"]["labels"]["apprafter.io/environment"],
            "prod"
        );
        let names: Vec<&str> = reg["status"]["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .map(|w| {
                assert_eq!(w["group"], "apprafter.io");
                assert_eq!(w["kind"], "Application");
                w["name"].as_str().expect("a name")
            })
            .collect();
        assert_eq!(names, ["shop-prod", "worker"]);
    }
}
