// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The core's events as the webview receives them.
//!
//! [`OpReporter`] is the `apprafter_core::Reporter` an operation runs with. Tool output is
//! decoded per stream ([`Utf8Stream`]) and coalesced, so a chatty tool costs a few IPC
//! messages rather than one per write; every other event first sends the output that came
//! before it, so the page never shows a warning ahead of the lines that led to it.

use std::sync::{Arc, Mutex, MutexGuard};

use apprafter_core::{Event, Reporter, Stream};
use apprafter_desktop_ipc::{OpEvent, OutputStream};

use super::{Clock, Utf8Stream};

/// Pending text that reaches this size goes out at once, in pieces no larger: a chatty tool
/// costs few IPC messages, and no message (or replay eviction) is ever large.
pub const FLUSH_BYTES: usize = 8 * 1024;

/// Pending text that has waited this long goes out however small: short enough that a
/// person reads the output as live.
pub const FLUSH_AGE_MS: u64 = 50;

/// Where an [`OpReporter`] sends what it converts (the operation manager's fan-out).
pub type Sink = Box<dyn Fn(OpEvent) + Send + Sync>;

/// Converts and coalesces one operation's events; shared by the operation's thread (which
/// reports) and the manager's ticker (which calls [`flush_due`](Self::flush_due)).
///
/// The sink runs with this reporter's lock held: that is what keeps one order between the
/// two callers. So the sink must never call back into its reporter, and nobody may call the
/// reporter while holding a lock the sink takes.
pub struct OpReporter {
    clock: Arc<dyn Clock>,
    sink: Sink,
    streams: Mutex<Streams>,
}

/// One per output stream: decoding and coalescing never mix the two.
#[derive(Default)]
struct Streams {
    stdout: Pending,
    stderr: Pending,
}

impl Streams {
    fn get(&mut self, stream: OutputStream) -> &mut Pending {
        match stream {
            OutputStream::Stdout => &mut self.stdout,
            OutputStream::Stderr => &mut self.stderr,
        }
    }
}

#[derive(Default)]
struct Pending {
    decoder: Utf8Stream,
    text: String,
    /// When the oldest byte of `text` arrived.
    since_ms: u64,
}

impl Pending {
    /// Add decoded text; while [`FLUSH_BYTES`] of it are pending, send that much.
    fn append(&mut self, text: &str, now_ms: u64, mut send: impl FnMut(String)) {
        if text.is_empty() {
            return;
        }
        if self.text.is_empty() {
            self.since_ms = now_ms;
        }
        self.text.push_str(text);
        let mut start = 0;
        while self.text.len() - start >= FLUSH_BYTES {
            let end = start + floor_char_boundary(&self.text[start..], FLUSH_BYTES);
            send(self.text[start..end].to_owned());
            start = end;
        }
        if start > 0 {
            self.text.drain(..start);
            // Pending text was under FLUSH_BYTES before this call, so the first piece took
            // all of it: what is left arrived now.
            self.since_ms = now_ms;
        }
    }

    fn take(&mut self) -> Option<String> {
        (!self.text.is_empty()).then(|| std::mem::take(&mut self.text))
    }
}

/// The largest index `<= max` that starts a character (`str::floor_char_boundary` is newer
/// than the workspace's rust-version). `s.len() >= max` here.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    (0..=max)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0)
}

fn output_stream(stream: Stream) -> OutputStream {
    match stream {
        Stream::Stdout => OutputStream::Stdout,
        Stream::Stderr => OutputStream::Stderr,
    }
}

impl OpReporter {
    pub fn new(clock: Arc<dyn Clock>, sink: Sink) -> Self {
        Self {
            clock,
            sink,
            streams: Mutex::default(),
        }
    }

    /// Send the text of each stream that has waited [`FLUSH_AGE_MS`] by `now_ms`, stdout
    /// first. A clock that stepped back sends too, rather than hold text until it catches up.
    pub fn flush_due(&self, now_ms: u64) {
        let mut streams = self.lock();
        for stream in [OutputStream::Stdout, OutputStream::Stderr] {
            let pending = streams.get(stream);
            let waited = now_ms.saturating_sub(pending.since_ms);
            if waited >= FLUSH_AGE_MS || now_ms < pending.since_ms {
                self.send_pending(stream, pending);
            }
        }
    }

    /// The operation ended: send everything, stdout first, an incomplete character as
    /// U+FFFD.
    pub fn finish(&self) {
        let now_ms = self.clock.now_ms();
        let mut streams = self.lock();
        for stream in [OutputStream::Stdout, OutputStream::Stderr] {
            let pending = streams.get(stream);
            let tail = pending.decoder.finish();
            pending.append(&tail, now_ms, |text| self.send_output(stream, text));
            self.send_pending(stream, pending);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Streams> {
        self.streams.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn send_output(&self, stream: OutputStream, text: String) {
        (self.sink)(OpEvent::Output { stream, text });
    }

    fn send_pending(&self, stream: OutputStream, pending: &mut Pending) {
        if let Some(text) = pending.take() {
            self.send_output(stream, text);
        }
    }
}

impl Reporter for OpReporter {
    fn report(&self, event: Event) {
        // Exhaustive on purpose: a new core event must not compile until it is handled here.
        let event = match event {
            Event::Output { stream, bytes } => {
                let stream = output_stream(stream);
                let now_ms = self.clock.now_ms();
                let mut streams = self.lock();
                let pending = streams.get(stream);
                let text = pending.decoder.push(&bytes);
                pending.append(&text, now_ms, |text| self.send_output(stream, text));
                return;
            }
            Event::Stage {
                index,
                total,
                title,
            } => OpEvent::Stage {
                index,
                total,
                title,
            },
            Event::Progress { done, total, unit } => OpEvent::Progress { done, total, unit },
            Event::Warning { message } => OpEvent::Warning { message },
            Event::Notice { message } => OpEvent::Notice { message },
        };
        let mut streams = self.lock();
        for stream in [OutputStream::Stdout, OutputStream::Stderr] {
            self.send_pending(stream, streams.get(stream));
        }
        (self.sink)(event);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use apprafter_core::{Event, Reporter, Stream};
    use apprafter_desktop_ipc::{OpEvent, OutputStream};

    use super::{OpReporter, FLUSH_AGE_MS, FLUSH_BYTES};
    use crate::ops::test_clock::ManualClock;

    type Seen = Arc<Mutex<Vec<OpEvent>>>;

    fn reporter_at(ms: u64) -> (OpReporter, Arc<ManualClock>, Seen) {
        let clock = Arc::new(ManualClock::at(ms));
        let seen: Seen = Arc::default();
        let into = seen.clone();
        let r = OpReporter::new(
            clock.clone(),
            Box::new(move |e| into.lock().unwrap().push(e)),
        );
        (r, clock, seen)
    }

    fn take(seen: &Seen) -> Vec<OpEvent> {
        std::mem::take(&mut *seen.lock().unwrap())
    }

    fn stdout(bytes: &[u8]) -> Event {
        Event::Output {
            stream: Stream::Stdout,
            bytes: bytes.to_vec(),
        }
    }

    fn stderr(bytes: &[u8]) -> Event {
        Event::Output {
            stream: Stream::Stderr,
            bytes: bytes.to_vec(),
        }
    }

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

    fn warning(message: &str) -> Event {
        Event::Warning {
            message: message.into(),
        }
    }

    #[test]
    fn output_is_held_until_it_reaches_eight_kib() {
        let (r, _, seen) = reporter_at(0);
        r.report(stdout(&[b'a'; FLUSH_BYTES - 1]));
        assert_eq!(take(&seen), vec![]);
        r.report(stdout(b"b"));
        let mut expected = "a".repeat(FLUSH_BYTES - 1);
        expected.push('b');
        assert_eq!(take(&seen), vec![out(&expected)]);
        r.finish();
        assert_eq!(take(&seen), vec![], "nothing was left pending");
    }

    #[test]
    fn a_large_chunk_goes_out_in_pieces_no_larger_than_eight_kib() {
        let (r, _, seen) = reporter_at(0);
        let text = "€".repeat(7000); // 21000 bytes; 8 KiB is not a character boundary
        r.report(stdout(text.as_bytes()));
        let pieces = take(&seen);
        assert_eq!(pieces.len(), 2, "two full pieces now, the rest pending");
        for piece in &pieces {
            let OpEvent::Output { text, .. } = piece else {
                panic!("{piece:?}")
            };
            assert!(text.len() <= FLUSH_BYTES && text.len() > FLUSH_BYTES - 4);
        }
        r.finish();
        let all: String = pieces
            .into_iter()
            .chain(take(&seen))
            .map(|e| match e {
                OpEvent::Output { text, .. } => text,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(all, text);
    }

    #[test]
    fn a_warning_sends_the_pending_output_before_it() {
        let (r, _, seen) = reporter_at(0);
        r.report(stdout(b"pulling\n"));
        r.report(warning("slow registry"));
        assert_eq!(
            take(&seen),
            vec![
                out("pulling\n"),
                OpEvent::Warning {
                    message: "slow registry".into()
                }
            ]
        );
    }

    #[test]
    fn a_flush_sends_stdout_then_stderr_each_in_arrival_order() {
        let (r, _, seen) = reporter_at(0);
        r.report(stderr(b"e1 "));
        r.report(stdout(b"o1 "));
        r.report(stderr(b"e2"));
        r.report(stdout(b"o2"));
        r.report(Event::Notice {
            message: "n".into(),
        });
        assert_eq!(
            take(&seen),
            vec![
                out("o1 o2"),
                err("e1 e2"),
                OpEvent::Notice {
                    message: "n".into()
                }
            ]
        );
    }

    #[test]
    fn every_other_event_flushes_first_and_converts_one_to_one() {
        let cases = [
            (
                Event::Stage {
                    index: 2,
                    total: 5,
                    title: "Install".into(),
                },
                OpEvent::Stage {
                    index: 2,
                    total: 5,
                    title: "Install".into(),
                },
            ),
            (
                Event::Progress {
                    done: u64::MAX,
                    total: Some(u64::MAX - 1),
                    unit: "B".into(),
                },
                OpEvent::Progress {
                    done: u64::MAX,
                    total: Some(u64::MAX - 1),
                    unit: "B".into(),
                },
            ),
            (
                Event::Progress {
                    done: 1 << 40,
                    total: None,
                    unit: "objects".into(),
                },
                OpEvent::Progress {
                    done: 1 << 40,
                    total: None,
                    unit: "objects".into(),
                },
            ),
            (
                warning("w"),
                OpEvent::Warning {
                    message: "w".into(),
                },
            ),
            (
                Event::Notice {
                    message: "n".into(),
                },
                OpEvent::Notice {
                    message: "n".into(),
                },
            ),
        ];
        for (event, converted) in cases {
            let (r, _, seen) = reporter_at(0);
            r.report(event.clone());
            assert_eq!(take(&seen), vec![converted.clone()], "nothing pending");
            r.report(stderr(b"x"));
            r.report(event);
            assert_eq!(take(&seen), vec![err("x"), converted]);
        }
    }

    #[test]
    fn stdout_and_stderr_never_mix() {
        let (r, _, seen) = reporter_at(0);
        let euro = "€".as_bytes();
        r.report(stdout(&euro[..1]));
        r.report(stderr(b"x"));
        r.report(stdout(&euro[1..]));
        r.report(stderr(&[0xff]));
        r.finish();
        assert_eq!(take(&seen), vec![out("€"), err("x\u{fffd}")]);
    }

    #[test]
    fn flush_due_sends_a_stream_once_its_text_has_waited_fifty_ms() {
        let (r, clock, seen) = reporter_at(1_000);
        r.report(stdout(b"a"));
        clock.set(1_030);
        r.report(stderr(b"b"));
        r.flush_due(1_000 + FLUSH_AGE_MS - 1);
        assert_eq!(take(&seen), vec![]);
        r.flush_due(1_000 + FLUSH_AGE_MS);
        assert_eq!(take(&seen), vec![out("a")]);
        r.flush_due(1_030 + FLUSH_AGE_MS - 1);
        assert_eq!(take(&seen), vec![]);
        r.flush_due(1_030 + FLUSH_AGE_MS);
        assert_eq!(take(&seen), vec![err("b")]);
        r.flush_due(10_000);
        assert_eq!(take(&seen), vec![], "nothing pending, nothing sent");
    }

    #[test]
    fn the_age_counts_from_the_first_pending_byte() {
        let (r, clock, seen) = reporter_at(1_000);
        r.report(stdout(b"a"));
        clock.advance(40);
        r.report(stdout(b"b"));
        r.flush_due(1_000 + FLUSH_AGE_MS);
        assert_eq!(
            take(&seen),
            vec![out("ab")],
            "more text does not restart the wait"
        );
        clock.set(2_000);
        r.report(stdout(b"c"));
        r.flush_due(2_000 + FLUSH_AGE_MS - 1);
        assert_eq!(
            take(&seen),
            vec![],
            "sent text does not age what comes after"
        );
        r.flush_due(2_000 + FLUSH_AGE_MS);
        assert_eq!(take(&seen), vec![out("c")]);
    }

    #[test]
    fn what_is_left_after_a_full_piece_ages_from_its_arrival() {
        let (r, clock, seen) = reporter_at(1_000);
        r.report(stdout(b"old"));
        clock.set(1_045);
        r.report(stdout(&[b'n'; FLUSH_BYTES]));
        assert_eq!(take(&seen).len(), 1, "one full piece");
        r.flush_due(1_000 + FLUSH_AGE_MS);
        assert_eq!(take(&seen), vec![], "the old text already went out");
        r.flush_due(1_045 + FLUSH_AGE_MS);
        assert_eq!(take(&seen), vec![out("nnn")]);
    }

    #[test]
    fn a_clock_that_stepped_back_flushes_rather_than_waits() {
        let (r, _, seen) = reporter_at(5_000);
        r.report(stdout(b"a"));
        r.flush_due(1_000);
        assert_eq!(take(&seen), vec![out("a")]);
    }

    #[test]
    fn finish_drains_a_held_tail_as_a_replacement_character() {
        let (r, _, seen) = reporter_at(0);
        let euro = "€".as_bytes();
        r.report(stdout(&[b'a', euro[0]]));
        r.report(stderr(&euro[..2]));
        r.flush_due(1_000);
        assert_eq!(take(&seen), vec![out("a")], "an incomplete character waits");
        r.finish();
        assert_eq!(take(&seen), vec![out("\u{fffd}"), err("\u{fffd}")]);
        r.finish();
        assert_eq!(take(&seen), vec![]);
    }

    #[test]
    fn it_is_a_core_reporter_usable_from_another_thread() {
        let (r, _, seen) = reporter_at(0);
        let r: Arc<dyn Reporter> = Arc::new(r);
        let on_thread = r.clone();
        std::thread::spawn(move || on_thread.report(warning("from a thread")))
            .join()
            .unwrap();
        assert_eq!(
            take(&seen),
            vec![OpEvent::Warning {
                message: "from a thread".into()
            }]
        );
    }

    #[test]
    fn the_sink_is_never_given_empty_output() {
        let (r, _, seen) = reporter_at(0);
        r.report(stdout(&"€".as_bytes()[..1]));
        r.report(stdout(b""));
        r.report(warning("w"));
        r.flush_due(10_000);
        let events = take(&seen);
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, OpEvent::Output { text, .. } if text.is_empty())),
            "{events:?}"
        );
    }
}
