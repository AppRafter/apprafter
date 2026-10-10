// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The shapes of core operations (ADR 0067 §2).
//!
//! A read returns a report. A mutation first returns a [`Plan`]; the client
//! confirms it in its own way (the CLI prompts or takes `--yes`, the desktop
//! shows the plan and, for a destructive one, asks for an OS authentication
//! gesture); then the mutation executes and returns an [`Outcome`].

use serde::Serialize;

/// How much a mutation can lose, which decides the confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PlanClass {
    /// Undone by running the opposite operation; no confirmation.
    Reversible,
    /// Changes something that matters but loses nothing; a plain confirm.
    Bounded,
    /// Deletes data or infrastructure; the full plan and an explicit gesture.
    Destructive,
}

/// One object a plan changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PlannedChange {
    /// What kind of object, e.g. `Target`, `ResourceClaim`.
    pub kind: String,
    /// Which one, e.g. `prod`, `billing-api/pg`.
    pub object: String,
    /// What happens to it, e.g. `delete`, `1.16.5 → 1.16.6`.
    pub change: String,
}

/// What a mutation will do, before it does it.
///
/// A plan serialises for display — class, title, changes — but never
/// deserialises: nothing outside Rust can hand a plan back to execute.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan<T> {
    pub class: PlanClass,
    pub title: String,
    pub changes: Vec<PlannedChange>,
    /// The operation-specific data `execute` consumes. It never crosses
    /// IPC, so a webview can neither read it nor alter what runs after the
    /// confirmation; the desktop keeps the `Plan` in Rust, keyed by an
    /// operation id. It may hold secrets, and those must be
    /// [`SecretString`](crate::SecretString)s: `Debug` prints the payload.
    #[serde(skip)]
    pub payload: T,
}

/// How an executed operation ended.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome<T> {
    Completed {
        result: T,
    },
    /// Stopped by its cancellation token. `cleaned` lists what it removed
    /// on the way out, `left` what it could not.
    Cancelled {
        cleaned: Vec<String>,
        left: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_and_outcome_serialise_with_stable_tags() {
        let plan = Plan {
            class: PlanClass::Destructive,
            title: "Remove target prod".into(),
            changes: vec![PlannedChange {
                kind: "Target".into(),
                object: "prod".into(),
                change: "delete".into(),
            }],
            payload: (),
        };
        let v = serde_json::to_value(&plan).unwrap();
        assert_eq!(v["class"], "destructive");
        assert_eq!(v["changes"][0]["change"], "delete");

        let done: Outcome<u8> = Outcome::Completed { result: 7 };
        assert_eq!(serde_json::to_value(&done).unwrap()["status"], "completed");
        let stopped: Outcome<u8> = Outcome::Cancelled {
            cleaned: vec!["pod/a".into()],
            left: vec![],
        };
        assert_eq!(
            serde_json::to_value(&stopped).unwrap()["status"],
            "cancelled"
        );
    }

    #[cfg(feature = "ts")]
    #[test]
    fn plan_parts_and_outcome_have_typescript_declarations() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::new().with_large_int("number");
        let class = PlanClass::decl(&cfg);
        assert!(
            class.contains("\"reversible\"") && class.contains("\"destructive\""),
            "{class}"
        );
        let change = PlannedChange::decl(&cfg);
        assert!(
            change.contains("kind: string") && change.contains("change: string"),
            "{change}"
        );
        let outcome = Outcome::<()>::decl(&cfg);
        assert!(
            outcome.contains("\"status\": \"completed\"")
                || outcome.contains("status: \"completed\""),
            "{outcome}"
        );
    }

    /// What `execute` would consume: deliberately not `Serialize`.
    #[derive(Debug, Clone, PartialEq)]
    struct Secretive {
        token: crate::SecretString,
    }

    #[test]
    fn a_plan_serialises_without_its_payload() {
        let plan = Plan {
            class: PlanClass::Bounded,
            title: "Rotate the token".into(),
            changes: vec![],
            payload: Secretive {
                token: crate::SecretString::new("hunter2"),
            },
        };
        let v = serde_json::to_value(&plan).unwrap();
        assert!(v.get("payload").is_none(), "payload crossed IPC: {v}");
        assert_eq!(v["title"], "Rotate the token");
        assert!(!v.to_string().contains("hunter2"));
        assert!(!format!("{plan:?}").contains("hunter2"));
    }

    /// `Plan` must not implement `Deserialize`: a webview must not be able
    /// to hand back a plan to execute. This fails to compile if it does —
    /// the two blanket impls then both apply and the call is ambiguous.
    #[test]
    fn a_plan_cannot_be_deserialised() {
        trait AmbiguousIfDeserialize<A> {
            fn check() {}
        }
        impl<T> AmbiguousIfDeserialize<()> for T {}
        struct Invalid;
        impl<T: for<'de> serde::Deserialize<'de>> AmbiguousIfDeserialize<Invalid> for T {}
        <Plan<()> as AmbiguousIfDeserialize<_>>::check();
    }
}
