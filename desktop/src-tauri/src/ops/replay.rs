// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What an operation has reported so far, kept so a page that reloads or re-attaches can
//! catch up before it receives live events.
//!
//! Only tool output is bounded: when the text of the kept `Output` events passes the cap,
//! the oldest of them go and their bytes are counted. Stages, warnings, notices, the latest
//! progress and the final event always stay — they are what the page needs to show where
//! the operation is. An earlier progress is superseded by the next one, so frequent
//! progress never grows the buffer.

use std::collections::VecDeque;

use apprafter_desktop_ipc::OpEvent;

/// The cap the operation manager gives each operation's buffer.
pub const REPLAY_CAP: usize = 1 << 20;

#[derive(Debug)]
pub struct ReplayBuffer {
    cap: usize,
    events: VecDeque<OpEvent>,
    output_bytes: usize,
    dropped: u64,
}

impl ReplayBuffer {
    /// A buffer that keeps at most `cap` bytes of output text.
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            events: VecDeque::new(),
            output_bytes: 0,
            dropped: 0,
        }
    }

    /// Keep `event`; drop the oldest output while the kept output text exceeds the cap.
    /// Output goes whole, so the kept text never exceeds the cap — an `Output` larger than
    /// the cap on its own is dropped too. A `Progress` replaces the one before it, so an
    /// operation that reports progress all the time keeps one, at the place it arrived.
    pub fn push(&mut self, event: OpEvent) {
        match &event {
            OpEvent::Output { text, .. } => self.output_bytes += text.len(),
            OpEvent::Progress { .. } => {
                // At most one is kept, and usually near the back: searching from there
                // costs the events since the previous progress.
                let previous = self
                    .events
                    .iter()
                    .rposition(|e| matches!(e, OpEvent::Progress { .. }));
                if let Some(i) = previous {
                    self.events.remove(i);
                }
            }
            _ => {}
        }
        self.events.push_back(event);
        if self.output_bytes <= self.cap {
            return;
        }
        let Self {
            cap,
            events,
            output_bytes,
            dropped,
        } = self;
        events.retain(|e| match e {
            OpEvent::Output { text, .. } if *output_bytes > *cap => {
                *output_bytes -= text.len();
                *dropped += text.len() as u64;
                false
            }
            _ => true,
        });
    }

    /// What a page that attaches now replays: `OutputDropped` first when anything was
    /// dropped, then the kept events in the order they arrived.
    pub fn snapshot(&self) -> Vec<OpEvent> {
        let marker = (self.dropped > 0).then_some(OpEvent::OutputDropped {
            bytes: self.dropped,
        });
        marker
            .into_iter()
            .chain(self.events.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use apprafter_core::{Outcome, UiError};
    use apprafter_desktop_ipc::{OpEvent, OutputStream};
    use serde_json::json;

    use super::ReplayBuffer;

    fn out(text: &str) -> OpEvent {
        OpEvent::Output {
            stream: OutputStream::Stdout,
            text: text.into(),
        }
    }

    fn err(text: &str) -> OpEvent {
        OpEvent::Output {
            stream: OutputStream::Stderr,
            text: text.into(),
        }
    }

    fn stage(index: u32) -> OpEvent {
        OpEvent::Stage {
            index,
            total: 3,
            title: format!("step {index}"),
        }
    }

    fn kept_output_bytes(events: &[OpEvent]) -> usize {
        events
            .iter()
            .map(|e| match e {
                OpEvent::Output { text, .. } => text.len(),
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn events_come_back_in_the_order_they_arrived() {
        let mut buf = ReplayBuffer::new(1024);
        let events = vec![
            stage(1),
            out("a"),
            err("b"),
            OpEvent::Progress {
                done: 1,
                total: Some(2),
                unit: "B".into(),
            },
            out("c"),
            OpEvent::Warning {
                message: "w".into(),
            },
            stage(2),
        ];
        for e in events.clone() {
            buf.push(e);
        }
        assert_eq!(buf.snapshot(), events);
    }

    #[test]
    fn output_past_the_cap_drops_the_oldest_output() {
        let mut buf = ReplayBuffer::new(10);
        buf.push(out("aaaa"));
        buf.push(err("bbbb"));
        assert_eq!(buf.snapshot(), vec![out("aaaa"), err("bbbb")]);
        buf.push(out("cccc"));
        assert_eq!(
            buf.snapshot(),
            vec![
                OpEvent::OutputDropped { bytes: 4 },
                err("bbbb"),
                out("cccc")
            ]
        );
    }

    #[test]
    fn output_exactly_at_the_cap_is_all_kept() {
        let mut buf = ReplayBuffer::new(8);
        buf.push(out("aaaa"));
        buf.push(out("bbbb"));
        assert_eq!(buf.snapshot(), vec![out("aaaa"), out("bbbb")]);
    }

    #[test]
    fn one_output_larger_than_the_cap_is_dropped_whole() {
        let mut buf = ReplayBuffer::new(4);
        buf.push(stage(1));
        buf.push(out("abcdef"));
        assert_eq!(
            buf.snapshot(),
            vec![OpEvent::OutputDropped { bytes: 6 }, stage(1)]
        );
    }

    #[test]
    fn non_output_events_survive_eviction() {
        let failed = OpEvent::Failed {
            error: UiError {
                code: None,
                message: "boom".into(),
                help: None,
                causes: vec![],
                fields: Default::default(),
            },
        };
        let finished = OpEvent::Finished {
            outcome: Outcome::Completed { result: json!(1) },
        };
        let progress = OpEvent::Progress {
            done: 9,
            total: None,
            unit: "B".into(),
        };
        let warning = OpEvent::Warning {
            message: "w".into(),
        };
        let notice = OpEvent::Notice {
            message: "n".into(),
        };
        let mut buf = ReplayBuffer::new(4);
        for e in [
            stage(1),
            out("aaaa"),
            progress.clone(),
            warning.clone(),
            err("bbbb"),
            notice.clone(),
            finished.clone(),
            failed.clone(),
            out("cccc"),
        ] {
            buf.push(e);
        }
        assert_eq!(
            buf.snapshot(),
            vec![
                OpEvent::OutputDropped { bytes: 8 },
                stage(1),
                progress,
                warning,
                notice,
                finished,
                failed,
                out("cccc"),
            ]
        );
    }

    fn progress(done: u64) -> OpEvent {
        OpEvent::Progress {
            done,
            total: Some(10_000),
            unit: "B".into(),
        }
    }

    #[test]
    fn only_the_latest_progress_is_kept_where_it_arrived() {
        let warning = OpEvent::Warning {
            message: "w".into(),
        };
        let mut buf = ReplayBuffer::new(1024);
        buf.push(stage(1));
        for done in 0..5_000 {
            buf.push(progress(done));
        }
        buf.push(warning.clone());
        for done in 5_000..10_000 {
            buf.push(progress(done));
            if done == 7_000 {
                buf.push(out("x"));
            }
        }
        buf.push(stage(2));
        assert_eq!(
            buf.snapshot(),
            vec![stage(1), warning, out("x"), progress(9_999), stage(2)]
        );
        // A progress after the last stage moves to the end again.
        buf.push(progress(10_000));
        assert_eq!(buf.snapshot().last(), Some(&progress(10_000)));
        let kept = buf
            .snapshot()
            .iter()
            .filter(|e| matches!(e, OpEvent::Progress { .. }))
            .count();
        assert_eq!(kept, 1);
    }

    #[test]
    fn the_dropped_marker_appears_once_and_counts_exactly() {
        let cap = 16;
        let mut buf = ReplayBuffer::new(cap);
        let mut total = 0;
        for i in 0..50 {
            // Mixed widths: one byte, a three-byte euro sign, and U+FFFD (three bytes).
            let text = match i % 3 {
                0 => "x".repeat(i % 7 + 1),
                1 => "€".repeat(i % 4 + 1),
                _ => "\u{fffd}".into(),
            };
            total += text.len();
            buf.push(if i % 2 == 0 { out(&text) } else { err(&text) });
            if i % 10 == 0 {
                buf.push(stage(i as u32));
            }
        }
        let snapshot = buf.snapshot();
        let markers: Vec<u64> = snapshot
            .iter()
            .filter_map(|e| match e {
                OpEvent::OutputDropped { bytes } => Some(*bytes),
                _ => None,
            })
            .collect();
        assert_eq!(markers.len(), 1, "{snapshot:?}");
        assert!(matches!(snapshot[0], OpEvent::OutputDropped { .. }));
        let kept = kept_output_bytes(&snapshot);
        assert!(kept <= cap, "kept {kept} bytes of output, cap {cap}");
        assert_eq!(markers[0] as usize + kept, total);
        let stages = snapshot
            .iter()
            .filter(|e| matches!(e, OpEvent::Stage { .. }))
            .count();
        assert_eq!(stages, 5, "every stage survives");
        // Asking twice changes nothing: the marker is part of the view, not the buffer.
        assert_eq!(buf.snapshot(), snapshot);
    }
}
