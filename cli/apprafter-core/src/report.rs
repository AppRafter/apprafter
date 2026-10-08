// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Progress and captured tool output, reported as events (ADR 0067 §2).
//!
//! The CLI's reporter writes them to the terminal byte for byte as it
//! printed before; the desktop's forwards them to the UI.

use std::sync::Mutex;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One thing an operation wants its user to see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Step `index` of `total` begins.
    Stage {
        index: u32,
        total: u32,
        title: String,
    },
    /// Progress within a stage; `total` when known.
    Progress {
        done: u64,
        total: Option<u64>,
        unit: String,
    },
    /// Raw bytes a child process wrote, unsplit, in arrival order.
    Output {
        stream: Stream,
        bytes: Vec<u8>,
    },
    Warning {
        message: String,
    },
    Notice {
        message: String,
    },
}

/// Where an operation sends its events. Shared across threads.
pub trait Reporter: Send + Sync {
    fn report(&self, event: Event);
}

/// Drops every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullReporter;

impl Reporter for NullReporter {
    fn report(&self, _event: Event) {}
}

/// Keeps every event, in order (tests, and callers that render at the end).
#[derive(Debug, Default)]
pub struct CollectReporter {
    events: Mutex<Vec<Event>>,
}

impl CollectReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything reported so far, leaving the reporter empty.
    pub fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

impl Reporter for CollectReporter {
    fn report(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_reporter_keeps_order_and_empties_on_take() {
        let r = CollectReporter::new();
        r.report(Event::Stage {
            index: 1,
            total: 2,
            title: "apply".into(),
        });
        r.report(Event::Output {
            stream: Stream::Stderr,
            bytes: b"x\r".to_vec(),
        });
        let got = r.take();
        assert_eq!(got.len(), 2);
        assert!(matches!(got[0], Event::Stage { index: 1, .. }));
        assert!(r.take().is_empty());
    }

    #[test]
    fn events_serialise_with_a_type_tag() {
        let v = serde_json::to_value(Event::Notice {
            message: "hi".into(),
        })
        .unwrap();
        assert_eq!(v["type"], "notice");
        let v = serde_json::to_value(Event::Output {
            stream: Stream::Stdout,
            bytes: vec![104],
        })
        .unwrap();
        assert_eq!(v["stream"], "stdout");
    }

    #[test]
    fn reporters_are_object_safe_and_shareable() {
        fn takes(_: &dyn Reporter) {}
        takes(&NullReporter);
        let shared: std::sync::Arc<dyn Reporter> = std::sync::Arc::new(CollectReporter::new());
        let s2 = shared.clone();
        std::thread::spawn(move || {
            s2.report(Event::Notice {
                message: "t".into(),
            })
        })
        .join()
        .unwrap();
    }
}
