// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Operations as the webview sees them: a plan to confirm, then a stream of events.
//!
//! The plan itself never crosses IPC (its payload stays in Rust, keyed by [`OpId`]); the
//! webview gets a [`PlanView`] and later hands back only the id.

use apprafter_core::{Outcome, PlanClass, PlannedChange, UiError};
use serde::{Deserialize, Serialize};

/// Names one planned or running operation across IPC.
///
/// Ids stay below 2^53, so a JavaScript `number` holds them exactly (ts-rs: `number`).
/// On the wire it is the bare number both ways: serde_json writes and reads a newtype struct
/// as its inner value. No `#[serde(transparent)]`: ts-rs cannot parse it and warns on every
/// build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OpId(pub u64);

/// Which of a tool's output streams a chunk of text came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// One thing an operation reports to the webview, in order.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpEvent {
    /// Step `index` of `total` began.
    Stage {
        index: u32,
        total: u32,
        title: String,
    },
    /// `total` is `None` when the size is unknown.
    Progress {
        done: u64,
        total: Option<u64>,
        unit: String,
    },
    /// Tool output, decoded to text in Rust, one stream at a time.
    Output {
        stream: OutputStream,
        text: String,
    },
    /// Earlier output removed from the replay buffer (its cap); sent first on re-attach.
    OutputDropped {
        bytes: u64,
    },
    Warning {
        message: String,
    },
    Notice {
        message: String,
    },
    /// The operation ended: completed, or cancelled with what it cleaned up.
    Finished {
        outcome: Outcome<serde_json::Value>,
    },
    /// The operation ended with an error.
    Failed {
        error: UiError,
    },
}

/// What the webview shows to confirm a mutation; the plan itself stays in Rust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PlanView {
    /// What `op_execute` takes, once.
    pub op_id: OpId,
    pub class: PlanClass,
    pub title: String,
    pub changes: Vec<PlannedChange>,
    /// The target the operation acts on, when it has one.
    pub target: Option<String>,
    /// After this, in milliseconds since the Unix epoch, `op_execute` refuses the plan.
    pub expires_at_ms: u64,
}

/// Where an operation is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OpState {
    Running,
    Finished,
    Failed,
    Cancelled,
}

/// One row of `op_list`: the running and recently ended operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OpSummary {
    pub op_id: OpId,
    pub title: String,
    pub target: Option<String>,
    pub state: OpState,
    /// Milliseconds since the Unix epoch.
    pub started_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde_json::json;

    use super::*;

    fn wire(event: &OpEvent) -> String {
        serde_json::to_string(event).unwrap()
    }

    #[test]
    fn an_op_id_is_a_bare_number_both_ways() {
        assert_eq!(serde_json::to_string(&OpId(7)).unwrap(), "7");
        assert_eq!(serde_json::from_str::<OpId>("7").unwrap(), OpId(7));
    }

    #[test]
    fn output_carries_its_stream_and_decoded_text() {
        let event = OpEvent::Output {
            stream: OutputStream::Stderr,
            text: "x".into(),
        };
        assert_eq!(
            wire(&event),
            r#"{"kind":"output","stream":"stderr","text":"x"}"#
        );
        let event = OpEvent::Output {
            stream: OutputStream::Stdout,
            text: "y".into(),
        };
        assert_eq!(
            wire(&event),
            r#"{"kind":"output","stream":"stdout","text":"y"}"#
        );
    }

    #[test]
    fn progress_with_an_unknown_total_sends_null() {
        let event = OpEvent::Progress {
            done: 5,
            total: None,
            unit: "B".into(),
        };
        assert_eq!(
            wire(&event),
            r#"{"kind":"progress","done":5,"total":null,"unit":"B"}"#
        );
    }

    #[test]
    fn dropped_output_reports_its_byte_count() {
        assert_eq!(
            wire(&OpEvent::OutputDropped { bytes: 7 }),
            r#"{"kind":"output_dropped","bytes":7}"#
        );
    }

    #[test]
    fn stage_warning_and_notice_have_the_wire_shape() {
        let stage = OpEvent::Stage {
            index: 1,
            total: 3,
            title: "Pull".into(),
        };
        assert_eq!(
            wire(&stage),
            r#"{"kind":"stage","index":1,"total":3,"title":"Pull"}"#
        );
        assert_eq!(
            wire(&OpEvent::Warning {
                message: "w".into()
            }),
            r#"{"kind":"warning","message":"w"}"#
        );
        assert_eq!(
            wire(&OpEvent::Notice {
                message: "n".into()
            }),
            r#"{"kind":"notice","message":"n"}"#
        );
    }

    #[test]
    fn finished_nests_the_core_outcome() {
        let done = OpEvent::Finished {
            outcome: Outcome::Completed {
                result: json!({"a": 1}),
            },
        };
        assert_eq!(
            wire(&done),
            r#"{"kind":"finished","outcome":{"status":"completed","result":{"a":1}}}"#
        );
        let stopped = OpEvent::Finished {
            outcome: Outcome::Cancelled {
                cleaned: vec!["pod/a".into()],
                left: vec![],
            },
        };
        assert_eq!(
            wire(&stopped),
            r#"{"kind":"finished","outcome":{"status":"cancelled","cleaned":["pod/a"],"left":[]}}"#
        );
    }

    #[test]
    fn failed_carries_a_ui_error_object() {
        let error = UiError {
            code: Some(crate::errors::INTERNAL.into()),
            message: "boom".into(),
            help: None,
            causes: vec!["deeper".into()],
            fields: BTreeMap::new(),
        };
        let v = serde_json::to_value(OpEvent::Failed { error }).unwrap();
        assert_eq!(v["kind"], "failed");
        assert!(v["error"].is_object(), "{v}");
        assert_eq!(v["error"]["code"], "apprafter::desktop::internal");
        assert_eq!(v["error"]["message"], "boom");
        assert_eq!(v["error"]["causes"], json!(["deeper"]));
        assert_eq!(v["error"]["fields"], json!({}));
    }

    #[test]
    fn a_plan_view_has_camel_case_keys_and_no_payload() {
        let view = PlanView {
            op_id: OpId(7),
            class: PlanClass::Destructive,
            title: "Remove target prod".into(),
            changes: vec![PlannedChange {
                kind: "Target".into(),
                object: "prod".into(),
                change: "delete".into(),
            }],
            target: Some("prod".into()),
            expires_at_ms: 600_000,
        };
        let v = serde_json::to_value(&view).unwrap();
        let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            BTreeSet::from(["opId", "class", "title", "changes", "expiresAtMs", "target"])
        );
        assert_eq!(v["opId"], 7);
        assert_eq!(v["class"], "destructive");
        assert_eq!(v["changes"][0]["change"], "delete");
        assert_eq!(v["target"], "prod");
        assert_eq!(v["expiresAtMs"], 600_000);
    }

    #[test]
    fn an_op_summary_has_the_wire_shape() {
        let summary = OpSummary {
            op_id: OpId(3),
            title: "Upgrade".into(),
            target: None,
            state: OpState::Running,
            started_at_ms: 5,
        };
        assert_eq!(
            serde_json::to_string(&summary).unwrap(),
            r#"{"opId":3,"title":"Upgrade","target":null,"state":"running","startedAtMs":5}"#
        );
        for (state, wire) in [
            (OpState::Running, "running"),
            (OpState::Finished, "finished"),
            (OpState::Failed, "failed"),
            (OpState::Cancelled, "cancelled"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), wire);
        }
    }

    #[cfg(feature = "ts")]
    #[test]
    fn the_typescript_declarations_keep_numbers_and_tags() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::new().with_large_int("number");
        let op_id = OpId::decl(&cfg);
        assert!(op_id.contains("number"), "{op_id}");
        let event = OpEvent::decl(&cfg);
        for part in ["\"output_dropped\"", "\"finished\"", "done: number"] {
            assert!(event.contains(part), "{part} missing from {event}");
        }
        assert!(event.contains("\"kind\": \"output\"") || event.contains("kind: \"output\""));
        let view = PlanView::decl(&cfg);
        for field in ["opId: OpId", "expiresAtMs: number", "class: PlanClass"] {
            assert!(view.contains(field), "{field} missing from {view}");
        }
        for decl in [&op_id, &event, &view] {
            assert!(!decl.contains("bigint"), "{decl}");
        }
    }
}
