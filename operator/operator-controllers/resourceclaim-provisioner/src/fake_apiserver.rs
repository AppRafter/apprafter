// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A stateful in-memory apiserver for the Dragonfly allocation tests (WI-402).
//!
//! The scripted responder in `route_apiserver` answers one canned body per
//! route. The allocation bugs need more than that: an allocation LIST has to
//! SEE the `status.dbnum` an earlier PATCH wrote, a retry has to read back the
//! checkpoint the previous pass left, and a test has to make ANOTHER object's
//! write land at a chosen moment — the late commit a dropped reconcile leaves
//! behind. So this one keeps the claims, snapshots and shared databases it is
//! given, applies writes to them, and answers LISTs from what it holds.
//!
//! Status writes are server-side applied, with as much SSA as the dragonfly
//! paths lean on: an apply REPLACES its field manager's set of status fields,
//! so a field that manager applied before and leaves out now is removed —
//! unless another manager also applied it — and `conditions` merge by `type`,
//! the list-map key both CRDs declare. A test can therefore see a write prune
//! (or fail to prune) exactly what it would on a real apiserver. Ownership is
//! tracked per top-level status field and per condition type. A fixture's
//! status is recorded as owned the way a live object's is — the scheduler's
//! `provider` and `Scheduled` condition, the provisioner's manager everything
//! else — so a test starts from what a cluster would hold, prunes included.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use kube::client::Body;
use kube::Client;
use serde_json::{json, Value};

/// The password every Secret GET answers with.
pub(crate) const ADMIN_PW: &str = "adminpw";

/// One request, as the apiserver saw it.
#[derive(Clone, Debug)]
pub(crate) struct Call {
    pub(crate) method: String,
    /// The path, without the query string.
    pub(crate) path: String,
    /// The `fieldManager` query parameter, when the request carried one.
    pub(crate) field_manager: Option<String>,
    /// `true` for a server-side apply (`application/apply-patch+yaml`).
    pub(crate) apply: bool,
    pub(crate) body: Value,
}

impl Call {
    /// A cluster-wide LIST of an `apprafter.io` resource, e.g. `"resourceclaims"`.
    pub(crate) fn is_cluster_list(&self, plural: &str) -> bool {
        self.method == "GET" && self.path == format!("/apis/apprafter.io/v1alpha1/{plural}")
    }

    /// A status write to the named `apprafter.io` object.
    pub(crate) fn is_status_patch(&self, plural: &str, name: &str) -> bool {
        self.method == "PATCH" && self.path.ends_with(&format!("/{plural}/{name}/status"))
    }
}

type Trigger = Box<dyn Fn(&Call) -> bool + Send>;
type Delay = Box<dyn Fn(&Call) -> Option<Duration> + Send>;

/// Which status fields each field manager owns, per object.
type Owners = BTreeMap<String, BTreeSet<String>>;

/// The scheduler's field manager and the claim status fields it writes; every
/// other status field of a fixture is the provisioner's ([`crate::FIELD_MANAGER`]).
const SCHEDULER_MANAGER: &str = "resourceclaim-scheduler";
const SCHEDULER_FIELDS: [&str; 2] = ["provider", "conditions[Scheduled]"];

#[derive(Default)]
struct State {
    claims: BTreeMap<(String, String), Value>,
    retained: BTreeMap<String, Value>,
    shared: BTreeMap<(String, String), Value>,
    providers: Vec<Value>,
    /// Keyed by `"<plural>/<namespace>/<name>"`.
    owners: BTreeMap<String, Owners>,
    calls: Vec<Call>,
    /// Claims inserted the first time a matching request arrives, BEFORE that
    /// request is answered — so it, and everything after it, observes them.
    late: Vec<(Trigger, Value)>,
    delay: Option<Delay>,
}

/// Cheap to clone: every clone serves and inspects the same state.
#[derive(Clone, Default)]
pub(crate) struct FakeApiserver {
    state: Arc<Mutex<State>>,
}

fn key_of(obj: &Value) -> (String, String) {
    (
        obj["metadata"]["namespace"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        obj["metadata"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

fn list(kind: &str, items: Vec<Value>) -> (u16, Value) {
    (
        200,
        json!({
            "apiVersion": "apprafter.io/v1alpha1",
            "kind": format!("{kind}List"),
            "metadata": {},
            "items": items,
        }),
    )
}

fn failure(code: u16, reason: &str, message: String) -> (u16, Value) {
    (
        code,
        json!({
            "kind": "Status", "apiVersion": "v1", "status": "Failure",
            "reason": reason, "message": message, "code": code,
        }),
    )
}

fn not_found() -> (u16, Value) {
    failure(404, "NotFound", "not found".to_string())
}

fn found(obj: Option<Value>) -> (u16, Value) {
    obj.map_or_else(not_found, |o| (200, o))
}

/// The ownable status fields a body sends: each top-level key, and one
/// `conditions[<type>]` per condition (`conditions` is a list-map keyed by
/// `type`).
fn status_fields(status: &Value) -> BTreeSet<String> {
    let mut fields = BTreeSet::new();
    for (k, v) in status.as_object().into_iter().flatten() {
        if k == "conditions" {
            for c in v.as_array().into_iter().flatten() {
                fields.insert(format!("conditions[{}]", c["type"].as_str().unwrap_or("")));
            }
        } else {
            fields.insert(k.clone());
        }
    }
    fields
}

fn remove_status_field(status: &mut Value, field: &str) {
    if let Some(t) = field
        .strip_prefix("conditions[")
        .and_then(|f| f.strip_suffix(']'))
    {
        if let Some(conds) = status["conditions"].as_array_mut() {
            conds.retain(|c| c["type"].as_str() != Some(t));
        }
    } else if let Some(map) = status.as_object_mut() {
        map.remove(field);
    }
}

/// Server-side apply of `body.status` to `obj` by `manager`.
fn apply_status(obj: &mut Value, owners: &mut Owners, manager: &str, body: &Value) {
    let sent = body.get("status").cloned().unwrap_or_else(|| json!({}));
    let now = status_fields(&sent);
    let before = owners.remove(manager).unwrap_or_default();
    let elsewhere: BTreeSet<String> = owners.values().flatten().cloned().collect();
    if !obj["status"].is_object() {
        obj["status"] = json!({});
    }
    for gone in before.difference(&now) {
        if !elsewhere.contains(gone) {
            remove_status_field(&mut obj["status"], gone);
        }
    }
    for (k, v) in sent.as_object().into_iter().flatten() {
        if k != "conditions" {
            obj["status"][k] = v.clone();
            continue;
        }
        if !obj["status"]["conditions"].is_array() {
            obj["status"]["conditions"] = json!([]);
        }
        for c in v.as_array().into_iter().flatten() {
            let conds = obj["status"]["conditions"].as_array_mut().expect("array");
            match conds.iter_mut().find(|x| x["type"] == c["type"]) {
                Some(existing) => *existing = c.clone(),
                None => conds.push(c.clone()),
            }
        }
    }
    owners.insert(manager.to_string(), now);
}

/// Apply a finalizer merge-patch to `obj`.
fn merge_finalizers(obj: &mut Value, body: &Value) {
    if let Some(f) = body.pointer("/metadata/finalizers") {
        obj["metadata"]["finalizers"] = f.clone();
    }
}

impl State {
    /// Store a claim, recording its status as the scheduler and the
    /// provisioner would own it.
    fn insert_claim(&mut self, claim: Value) {
        let (ns, name) = key_of(&claim);
        let (scheduler, provisioner): (BTreeSet<String>, BTreeSet<String>) =
            status_fields(&claim["status"])
                .into_iter()
                .partition(|f| SCHEDULER_FIELDS.contains(&f.as_str()));
        self.owners.insert(
            format!("resourceclaims/{ns}/{name}"),
            Owners::from([
                (SCHEDULER_MANAGER.to_string(), scheduler),
                (crate::FIELD_MANAGER.to_string(), provisioner),
            ]),
        );
        self.claims.insert((ns, name), claim);
    }

    /// Store a shared database; the provisioner is the only writer of its
    /// status.
    fn insert_shared(&mut self, sd: Value) {
        let (ns, name) = key_of(&sd);
        self.owners.insert(
            format!("shareddatabases/{ns}/{name}"),
            Owners::from([(
                crate::FIELD_MANAGER.to_string(),
                status_fields(&sd["status"]),
            )]),
        );
        self.shared.insert((ns, name), sd);
    }

    /// A status write: server-side applies only — nothing on the paths under
    /// test writes status any other way, and a merge would need semantics this
    /// fake does not model.
    fn write_status(&mut self, call: &Call, plural: &str, ns: &str, name: &str) -> (u16, Value) {
        let Some(manager) = call.field_manager.clone().filter(|_| call.apply) else {
            return failure(
                500,
                "InternalError",
                format!("fake apiserver: status of {plural}/{name} written without an apply"),
            );
        };
        let key = (ns.to_string(), name.to_string());
        let obj = match plural {
            "resourceclaims" => self.claims.get_mut(&key),
            _ => self.shared.get_mut(&key),
        };
        let owners = self
            .owners
            .entry(format!("{plural}/{ns}/{name}"))
            .or_default();
        found(obj.map(|o| {
            apply_status(o, owners, &manager, &call.body);
            o.clone()
        }))
    }

    fn answer(&mut self, call: &Call) -> (u16, Value) {
        let seg: Vec<&str> = call.path.trim_start_matches('/').split('/').collect();
        let key = |ns: &str, name: &str| (ns.to_string(), name.to_string());
        match (call.method.as_str(), seg.as_slice()) {
            ("GET", ["api", "v1", "namespaces", ns, "secrets", name]) => (
                200,
                json!({
                    "apiVersion": "v1", "kind": "Secret",
                    "metadata": { "name": name, "namespace": ns },
                    "data": { "password": base64::engine::general_purpose::STANDARD.encode(ADMIN_PW) },
                }),
            ),
            ("DELETE", ["api", "v1", "namespaces", _, "secrets", _]) => not_found(),
            ("GET", ["apis", "apprafter.io", "v1alpha1", "serviceproviders"]) => {
                list("ServiceProvider", self.providers.clone())
            }
            ("GET", ["apis", "apprafter.io", "v1alpha1", "resourceclaims"]) => {
                list("ResourceClaim", self.claims.values().cloned().collect())
            }
            ("GET", ["apis", "apprafter.io", "v1alpha1", "namespaces", ns, "resourceclaims"]) => {
                list(
                    "ResourceClaim",
                    self.claims
                        .iter()
                        .filter(|((n, _), _)| n == ns)
                        .map(|(_, c)| c.clone())
                        .collect(),
                )
            }
            (
                "GET",
                ["apis", "apprafter.io", "v1alpha1", "namespaces", ns, "resourceclaims", name],
            ) => found(self.claims.get(&key(ns, name)).cloned()),
            (
                "PATCH",
                ["apis", "apprafter.io", "v1alpha1", "namespaces", ns, plural @ ("resourceclaims" | "shareddatabases"), name, "status"],
            ) => self.write_status(call, plural, ns, name),
            (
                "PATCH",
                ["apis", "apprafter.io", "v1alpha1", "namespaces", ns, "resourceclaims", name],
            ) => found(self.claims.get_mut(&key(ns, name)).map(|c| {
                merge_finalizers(c, &call.body);
                c.clone()
            })),
            ("GET", ["apis", "apprafter.io", "v1alpha1", "retainedclaims"])
            | ("GET", ["apis", "apprafter.io", "v1alpha1", "namespaces", _, "retainedclaims"]) => {
                list("RetainedClaim", self.retained.values().cloned().collect())
            }
            (
                "GET",
                ["apis", "apprafter.io", "v1alpha1", "namespaces", _, "retainedclaims", name],
            ) => found(self.retained.get(*name).cloned()),
            (
                "DELETE",
                ["apis", "apprafter.io", "v1alpha1", "namespaces", _, "retainedclaims", name],
            ) => found(self.retained.remove(*name)),
            ("GET", ["apis", "apprafter.io", "v1alpha1", "shareddatabases"]) => {
                list("SharedDatabase", self.shared.values().cloned().collect())
            }
            (
                "PATCH",
                ["apis", "apprafter.io", "v1alpha1", "namespaces", ns, "shareddatabases", name],
            ) => found(self.shared.get_mut(&key(ns, name)).map(|s| {
                merge_finalizers(s, &call.body);
                s.clone()
            })),
            // Secrets, the `Dragonfly` CR: an apply answers with what it sent.
            ("PATCH", _) => (200, call.body.clone()),
            _ => failure(
                500,
                "InternalError",
                format!("fake apiserver: no route for {} {}", call.method, call.path),
            ),
        }
    }
}

impl FakeApiserver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Serve a `ServiceProvider` of `type_` on `backend`, in `apprafter-system`.
    pub(crate) fn provider(&self, name: &str, type_: &str, backend: &str) -> &Self {
        self.state.lock().unwrap().providers.push(json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "ServiceProvider",
            "metadata": { "name": name, "namespace": "apprafter-system" },
            "spec": { "type": type_, "backend": backend },
        }));
        self
    }

    pub(crate) fn put_claim(&self, claim: Value) -> &Self {
        self.state.lock().unwrap().insert_claim(claim);
        self
    }

    pub(crate) fn put_retained(&self, rc: Value) -> &Self {
        let name = key_of(&rc).1;
        self.state.lock().unwrap().retained.insert(name, rc);
        self
    }

    pub(crate) fn put_shared(&self, sd: Value) -> &Self {
        self.state.lock().unwrap().insert_shared(sd);
        self
    }

    /// Insert `claim` the first time a request matching `when` arrives — the
    /// write of a reconcile that was dropped mid-request and committed late.
    pub(crate) fn commit_late(
        &self,
        when: impl Fn(&Call) -> bool + Send + 'static,
        claim: Value,
    ) -> &Self {
        self.state
            .lock()
            .unwrap()
            .late
            .push((Box::new(when), claim));
        self
    }

    /// Hold the response to every request `delay` names for that long. The
    /// request has already taken effect when the delay starts.
    pub(crate) fn delay(
        &self,
        delay: impl Fn(&Call) -> Option<Duration> + Send + 'static,
    ) -> &Self {
        self.state.lock().unwrap().delay = Some(Box::new(delay));
        self
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.state.lock().unwrap().calls.clone()
    }

    pub(crate) fn claim(&self, ns: &str, name: &str) -> Value {
        self.state.lock().unwrap().claims[&(ns.to_string(), name.to_string())].clone()
    }

    pub(crate) fn shared(&self, ns: &str, name: &str) -> Value {
        self.state.lock().unwrap().shared[&(ns.to_string(), name.to_string())].clone()
    }

    pub(crate) fn retained(&self, name: &str) -> Option<Value> {
        self.state.lock().unwrap().retained.get(name).cloned()
    }

    pub(crate) fn client(&self) -> Client {
        let state = self.state.clone();
        let service = tower::service_fn(move |req: http::Request<Body>| {
            let state = state.clone();
            async move {
                let method = req.method().to_string();
                let path = req.uri().path().to_string();
                let field_manager = req.uri().query().and_then(|q| {
                    q.split('&')
                        .find_map(|kv| kv.strip_prefix("fieldManager="))
                        .map(str::to_string)
                });
                let apply = req
                    .headers()
                    .get(http::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    == Some("application/apply-patch+yaml");
                let bytes = req.into_body().collect_bytes().await.expect("request body");
                let call = Call {
                    method,
                    path,
                    field_manager,
                    apply,
                    body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                };
                let (code, payload, delay) = {
                    let mut st = state.lock().unwrap();
                    let late = std::mem::take(&mut st.late);
                    for (when, claim) in late {
                        if when(&call) {
                            st.insert_claim(claim);
                        } else {
                            st.late.push((when, claim));
                        }
                    }
                    let (code, payload) = st.answer(&call);
                    let delay = st.delay.as_ref().and_then(|d| d(&call));
                    st.calls.push(call);
                    (code, payload, delay)
                };
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(code)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&payload).expect("json")))
                        .expect("response"),
                )
            }
        });
        Client::new(service, "apprafter-system")
    }
}

/// The harness's own semantics. The allocation tests' verdicts rest on these
/// behaviours, so they are pinned rather than assumed.
#[cfg(test)]
mod tests {
    use super::*;
    use kube::api::{Api, Patch, PatchParams};
    use operator_core::{ResourceClaim, RetainedClaim, ServiceProvider, SharedDatabase};

    fn claim(name: &str, dbnum: Option<u16>) -> Value {
        let mut c = json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "ResourceClaim",
            "metadata": { "name": name, "namespace": "apps" },
            "spec": { "type": "redis", "selector": {} },
        });
        if let Some(n) = dbnum {
            c["status"] = json!({ "instance": "i", "dbnum": n });
        }
        c
    }

    async fn listed_dbnums(client: &Client) -> Vec<Option<u16>> {
        Api::<ResourceClaim>::all(client.clone())
            .list(&Default::default())
            .await
            .unwrap()
            .items
            .iter()
            .map(|c| c.status.as_ref().and_then(|s| s.dbnum))
            .collect()
    }

    async fn apply_status(client: &Client, manager: &str, status: Value) {
        Api::<ResourceClaim>::namespaced(client.clone(), "apps")
            .patch_status(
                "a",
                &PatchParams::apply(manager),
                &Patch::Apply(&json!({
                    "apiVersion": "apprafter.io/v1alpha1", "kind": "ResourceClaim",
                    "metadata": { "name": "a" }, "status": status,
                })),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_status_write_is_what_the_next_list_returns() {
        let api = FakeApiserver::new();
        api.put_claim(claim("a", None));
        let client = api.client();
        apply_status(&client, "t", json!({ "dbnum": 3 })).await;
        assert_eq!(listed_dbnums(&client).await, vec![Some(3)]);
        assert_eq!(api.claim("apps", "a")["status"]["dbnum"], 3);
        let calls = api.calls();
        assert!(calls[0].is_status_patch("resourceclaims", "a"));
        assert_eq!(calls[0].field_manager.as_deref(), Some("t"));
        assert!(calls[0].apply);
        assert!(calls[1].is_cluster_list("resourceclaims"));
    }

    const T: &str = "2026-10-01T00:00:00Z";

    #[tokio::test]
    async fn an_apply_prunes_what_its_manager_stopped_sending_and_nothing_else() {
        let api = FakeApiserver::new();
        let mut c = claim("a", None);
        c["status"] = json!({
            "provider": "redis-df",
            "conditions": [{ "type": "Scheduled", "status": "True", "lastTransitionTime": T }],
        });
        api.put_claim(c);
        let client = api.client();
        apply_status(
            &client,
            "p",
            json!({ "instance": "i", "dbnum": 3, "ready": false }),
        )
        .await;
        apply_status(
            &client,
            "q",
            json!({ "ready": false, "size": { "bytes": 1 } }),
        )
        .await;
        let ready = |status: &str| {
            json!({
                "conditions": [{ "type": "Ready", "status": status, "lastTransitionTime": T }],
            })
        };
        apply_status(&client, "p", ready("False")).await;
        apply_status(&client, "p", ready("True")).await;
        let st = api.claim("apps", "a")["status"].clone();
        assert_eq!(st["instance"], Value::Null, "p's instance was pruned");
        assert_eq!(st["dbnum"], Value::Null, "p's dbnum was pruned");
        assert_eq!(st["ready"], false, "a field q also applied survives");
        assert_eq!(st["size"]["bytes"], 1, "another manager's field survives");
        assert_eq!(st["provider"], "redis-df", "the scheduler's field survives");
        let conds: Vec<(&str, &str)> = st["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["type"].as_str().unwrap(), c["status"].as_str().unwrap()))
            .collect();
        assert_eq!(
            conds,
            [("Scheduled", "True"), ("Ready", "True")],
            "conditions merge by type"
        );
    }

    #[tokio::test]
    async fn a_fixtures_status_is_owned_as_a_live_claims_would_be() {
        // The provisioner checkpointed `instance`/`dbnum` on a claim the
        // scheduler had placed; the provisioner's next apply that leaves the
        // checkpoint out removes it, and leaves the scheduler's fields alone.
        let api = FakeApiserver::new();
        let mut c = claim("a", Some(3));
        c["status"]["provider"] = json!("redis-df");
        c["status"]["conditions"] =
            json!([{ "type": "Scheduled", "status": "True", "lastTransitionTime": T }]);
        api.put_claim(c);
        apply_status(
            &api.client(),
            crate::FIELD_MANAGER,
            json!({ "ready": false }),
        )
        .await;
        let st = api.claim("apps", "a")["status"].clone();
        assert_eq!(st["dbnum"], Value::Null);
        assert_eq!(st["instance"], Value::Null);
        assert_eq!(st["provider"], "redis-df");
        assert_eq!(st["conditions"][0]["type"], "Scheduled");
    }

    #[tokio::test]
    async fn a_status_merge_patch_is_refused_rather_than_guessed_at() {
        let api = FakeApiserver::new();
        api.put_claim(claim("a", Some(3)));
        let err = Api::<ResourceClaim>::namespaced(api.client(), "apps")
            .patch_status(
                "a",
                &PatchParams::default(),
                &Patch::Merge(&json!({ "status": { "dbnum": null } })),
            )
            .await
            .expect_err("not modelled");
        assert!(err.to_string().contains("without an apply"), "{err}");
        assert_eq!(api.claim("apps", "a")["status"]["dbnum"], 3);
    }

    #[tokio::test]
    async fn a_late_commit_is_visible_to_the_request_that_triggers_it() {
        let api = FakeApiserver::new();
        api.commit_late(
            |c| c.is_cluster_list("resourceclaims"),
            claim("late", Some(0)),
        );
        let client = api.client();
        assert_eq!(listed_dbnums(&client).await, vec![Some(0)]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_delayed_write_has_taken_effect_before_it_is_answered() {
        let api = FakeApiserver::new();
        api.put_claim(claim("a", None)).delay(|c| {
            c.is_status_patch("resourceclaims", "a")
                .then_some(Duration::from_secs(10))
        });
        let client = api.client();
        let writer = client.clone();
        let write =
            tokio::spawn(async move { apply_status(&writer, "t", json!({ "dbnum": 5 })).await });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(
            !write.is_finished(),
            "the write is still waiting for its answer"
        );
        assert_eq!(listed_dbnums(&client).await, vec![Some(5)]);
        write.await.unwrap();
    }

    #[tokio::test]
    async fn snapshots_and_providers_are_served_and_a_snapshot_can_be_deleted() {
        let api = FakeApiserver::new();
        api.provider("redis-df", "redis", "dragonfly")
            .put_retained(json!({
                "apiVersion": "apprafter.io/v1alpha1", "kind": "RetainedClaim",
                "metadata": { "name": "apps-web", "namespace": "apprafter-system" },
                "spec": {
                    "claimRef": { "name": "web", "namespace": "apps" },
                    "provider": "redis-df", "backend": "dragonfly",
                    "retainUntil": "2099-01-01T00:00:00Z",
                },
            }));
        let client = api.client();
        let providers = Api::<ServiceProvider>::all(client.clone())
            .list(&Default::default())
            .await
            .unwrap()
            .items;
        assert_eq!(providers[0].spec.backend, "dragonfly");
        let rcs: Api<RetainedClaim> = Api::namespaced(client, "apprafter-system");
        assert!(rcs.get_opt("apps-web").await.unwrap().is_some());
        rcs.delete("apps-web", &Default::default()).await.unwrap();
        assert!(rcs.get_opt("apps-web").await.unwrap().is_none());
        assert!(api.retained("apps-web").is_none());
    }

    #[tokio::test]
    async fn shared_databases_are_listed_and_status_written() {
        let api = FakeApiserver::new();
        api.put_shared(json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "SharedDatabase",
            "metadata": { "name": "orders", "namespace": "apps" },
            "spec": { "type": "redis" },
        }));
        let client = api.client();
        Api::<SharedDatabase>::namespaced(client.clone(), "apps")
            .patch_status(
                "orders",
                &PatchParams::apply("t"),
                &Patch::Apply(&json!({
                    "apiVersion": "apprafter.io/v1alpha1", "kind": "SharedDatabase",
                    "metadata": { "name": "orders" }, "status": { "dbnum": 2 },
                })),
            )
            .await
            .unwrap();
        let listed = Api::<SharedDatabase>::all(client)
            .list(&Default::default())
            .await
            .unwrap()
            .items;
        assert_eq!(listed[0].status.as_ref().and_then(|s| s.dbnum), Some(2));
        assert_eq!(api.shared("apps", "orders")["status"]["dbnum"], 2);
    }
}
