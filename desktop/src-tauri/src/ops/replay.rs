// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What an operation has reported so far, kept so a page that reloads or re-attaches can
//! catch up before it receives live events.
//!
//! Every kind of event is bounded. Tool output: when the text of the kept `Output` events
//! passes the byte cap, the oldest of them go and their bytes are counted. Stages, warnings
//! and notices — messages: the newest [`MESSAGE_CAP`] stay and the earlier ones are counted.
//! A progress supersedes the one before it, so frequent progress never grows the buffer. The
//! final event always stays.
//!
//! A snapshot leads with what was dropped — `OutputDropped` first, where the wire type says
//! it goes, then a `Notice` saying how many messages went — and then has the kept events in
//! the order they arrived. Each kind waits in a queue of its own, tagged with its arrival,
//! and what goes is always the oldest of its kind: a push costs amortised O(1), and the
//! snapshot merges the queues back into arrival order.

use std::collections::VecDeque;
use std::iter::Peekable;

use apprafter_desktop_ipc::OpEvent;

/// The output cap the operation manager gives each operation's buffer.
pub const REPLAY_CAP: usize = 1 << 20;

/// How many messages (stages, warnings, notices) a buffer keeps.
pub const MESSAGE_CAP: usize = 1024;

/// An event and when it arrived, counting from 0.
type Arrived = (u64, OpEvent);

#[derive(Debug)]
pub struct ReplayBuffer {
    cap: usize,
    message_cap: usize,
    next: u64,
    outputs: VecDeque<Arrived>,
    output_bytes: usize,
    messages: VecDeque<Arrived>,
    progress: Option<Arrived>,
    /// `Finished` or `Failed`: one, at the end of an operation.
    last: Vec<Arrived>,
    dropped_bytes: u64,
    dropped_messages: u64,
}

/// How a kind of event is bounded.
enum Kind {
    /// Its text's length in bytes.
    Output(usize),
    Progress,
    Last,
    Message,
}

impl Kind {
    fn of(event: &OpEvent) -> Self {
        // Exhaustive on purpose: a new event must not compile until it is bounded here.
        match event {
            OpEvent::Output { text, .. } => Kind::Output(text.len()),
            OpEvent::Progress { .. } => Kind::Progress,
            OpEvent::Finished { .. } | OpEvent::Failed { .. } => Kind::Last,
            OpEvent::Stage { .. }
            | OpEvent::Warning { .. }
            | OpEvent::Notice { .. }
            | OpEvent::OutputDropped { .. } => Kind::Message,
        }
    }
}

impl ReplayBuffer {
    /// A buffer that keeps at most `cap` bytes of output text and [`MESSAGE_CAP`] messages.
    pub fn new(cap: usize) -> Self {
        Self::with_caps(cap, MESSAGE_CAP)
    }

    /// At most `cap` bytes of output text and `messages` messages.
    pub fn with_caps(cap: usize, messages: usize) -> Self {
        Self {
            cap,
            message_cap: messages,
            next: 0,
            outputs: VecDeque::new(),
            output_bytes: 0,
            messages: VecDeque::new(),
            progress: None,
            last: Vec::new(),
            dropped_bytes: 0,
            dropped_messages: 0,
        }
    }

    /// Keep `event`, and drop the oldest of its kind past the cap. Output goes whole, so the
    /// kept text never exceeds the cap — an `Output` larger than the cap on its own is
    /// dropped too. A `Progress` replaces the one before it, so an operation that reports
    /// progress all the time keeps one, at the place it arrived.
    pub fn push(&mut self, event: OpEvent) {
        let arrived = (self.next, event);
        self.next += 1;
        match Kind::of(&arrived.1) {
            Kind::Output(len) => {
                self.output_bytes += len;
                self.outputs.push_back(arrived);
                while self.output_bytes > self.cap {
                    let Some((_, oldest)) = self.outputs.pop_front() else {
                        break;
                    };
                    let len = match &oldest {
                        OpEvent::Output { text, .. } => text.len(),
                        _ => 0,
                    };
                    self.output_bytes -= len;
                    self.dropped_bytes += len as u64;
                }
            }
            Kind::Progress => self.progress = Some(arrived),
            Kind::Last => self.last.push(arrived),
            Kind::Message => {
                self.messages.push_back(arrived);
                if self.messages.len() > self.message_cap {
                    self.messages.pop_front();
                    self.dropped_messages += 1;
                }
            }
        }
    }

    /// What a page that attaches now replays: `OutputDropped` when output was dropped, then
    /// a `Notice` when messages were, then the kept events in the order they arrived.
    pub fn snapshot(&self) -> Vec<OpEvent> {
        let mut snapshot =
            Vec::with_capacity(2 + self.outputs.len() + self.messages.len() + self.last.len() + 1);
        if self.dropped_bytes > 0 {
            snapshot.push(OpEvent::OutputDropped {
                bytes: self.dropped_bytes,
            });
        }
        if self.dropped_messages > 0 {
            snapshot.push(OpEvent::Notice {
                message: match self.dropped_messages {
                    1 => "1 earlier message was dropped".into(),
                    n => format!("{n} earlier messages were dropped"),
                },
            });
        }
        let mut queues = [
            queue(self.outputs.iter()),
            queue(self.messages.iter()),
            queue(self.progress.iter()),
            queue(self.last.iter()),
        ];
        // Each queue is in arrival order: take the earliest head until all are empty.
        loop {
            let earliest = queues
                .iter_mut()
                .enumerate()
                .filter_map(|(i, queue)| queue.peek().map(|(at, _)| (*at, i)))
                .min();
            let Some((_, i)) = earliest else {
                break;
            };
            if let Some((_, event)) = queues[i].next() {
                snapshot.push(event.clone());
            }
        }
        snapshot
    }
}

/// One kind's events, oldest first, as the snapshot merges them.
type Queue<'a> = Peekable<Box<dyn Iterator<Item = &'a Arrived> + 'a>>;

fn queue<'a>(events: impl Iterator<Item = &'a Arrived> + 'a) -> Queue<'a> {
    let events: Box<dyn Iterator<Item = &'a Arrived> + 'a> = Box::new(events);
    events.peekable()
}

#[cfg(test)]
mod tests {
    use apprafter_core::{Outcome, UiError};
    use apprafter_desktop_ipc::{OpEvent, OutputStream};
    use serde_json::json;

    use super::{ReplayBuffer, MESSAGE_CAP, REPLAY_CAP};

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

    fn notice(message: &str) -> OpEvent {
        OpEvent::Notice {
            message: message.into(),
        }
    }

    fn warning(message: &str) -> OpEvent {
        OpEvent::Warning {
            message: message.into(),
        }
    }

    #[test]
    fn messages_past_their_cap_drop_the_oldest_and_a_notice_says_how_many() {
        let finished = OpEvent::Finished {
            outcome: Outcome::Completed { result: json!(1) },
        };
        let mut buf = ReplayBuffer::with_caps(1024, 3);
        for e in [
            stage(1),
            out("a"),
            warning("w"),
            progress(1),
            notice("n"),
            stage(2),
            stage(3),
            finished.clone(),
        ] {
            buf.push(e);
        }
        assert_eq!(
            buf.snapshot(),
            vec![
                notice("2 earlier messages were dropped"),
                out("a"),
                progress(1),
                notice("n"),
                stage(2),
                stage(3),
                finished,
            ],
            "output, the latest progress and the final event are not messages"
        );
    }

    #[test]
    fn with_both_dropped_the_output_marker_leads_then_the_messages_notice() {
        let mut buf = ReplayBuffer::with_caps(4, 1);
        for e in [out("aaaa"), stage(1), err("bbbb"), stage(2)] {
            buf.push(e);
        }
        let snapshot = buf.snapshot();
        assert_eq!(
            snapshot,
            vec![
                OpEvent::OutputDropped { bytes: 4 },
                notice("1 earlier message was dropped"),
                err("bbbb"),
                stage(2),
            ]
        );
        assert_eq!(buf.snapshot(), snapshot, "asking twice changes nothing");
    }

    #[test]
    fn the_operation_buffer_keeps_the_newest_1024_messages() {
        assert_eq!(MESSAGE_CAP, 1024);
        let mut buf = ReplayBuffer::new(REPLAY_CAP);
        let total = MESSAGE_CAP as u32 + 10;
        for index in 0..total {
            buf.push(stage(index));
            buf.push(out("x"));
        }
        let snapshot = buf.snapshot();
        assert_eq!(snapshot[0], notice("10 earlier messages were dropped"));
        let stages: Vec<u32> = snapshot
            .iter()
            .filter_map(|e| match e {
                OpEvent::Stage { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(stages, (10..total).collect::<Vec<_>>());
        assert_eq!(
            kept_output_bytes(&snapshot),
            total as usize,
            "no output dropped"
        );
        // Arrival order across the kinds: each kept stage is followed by its output.
        let after_marker = &snapshot[1..];
        assert_eq!(
            after_marker[0],
            out("x"),
            "the output of a dropped stage stays"
        );
        let i = after_marker.iter().position(|e| *e == stage(10)).unwrap();
        assert_eq!(after_marker[i + 1], out("x"));
        assert_eq!(after_marker.last(), Some(&out("x")));
    }
}
