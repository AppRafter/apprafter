// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The core is synchronous and deep (GOTCHA-67), so every thread it runs on has an 8 MiB
//! stack: the async runtime's threads, the blocking pool every command runs the core on
//! included (`runtime::init_runtime`), and every operation thread (`OperationManager`). Both
//! are checked here by force, not by sampling: [`needs_deep_stack`] needs about 4 MiB of stack
//! on every build — more than the 2 MiB a thread gets when nobody sizes it (std's default, and
//! tokio's, with `RUST_MIN_STACK` unset) and half of the 8 MiB, so it fits with room to spare
//! — and it measures what it used, so an optimiser that shrinks its frames fails the test
//! instead of passing it vacuously.
//!
//! A stack overflow does not fail a test, it aborts the test process: "thread '<name>' has
//! overflowed its stack" and "fatal runtime error: stack overflow", then SIGABRT. That abort
//! IS the failure signal, and the thread name in it says which case died: `tokio-runtime` is
//! the blocking pool, `op-<n>` an operation thread. Drop `thread_stack_size` from
//! `init_runtime` and the first dies; drop `stack_size` from the manager's `spawn_op` and the
//! second does.
//!
//! This binary must not touch `tauri::async_runtime` before `init_runtime` has run (Tauri
//! would build its default runtime, with default stacks): only the blocking-pool test does, and
//! it calls `init_runtime` first.

use std::hint::black_box;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use apprafter_core::{Outcome, PlanClass};
use apprafter_desktop::auth::NoAuthenticator;
use apprafter_desktop::ops::{EventSink, OperationManager, PlanParts, SystemClock};
use apprafter_desktop::runtime::{self, THREAD_STACK_BYTES};
use apprafter_desktop_ipc::OpEvent;
use serde_json::json;

/// What one level of [`descend`] keeps on its frame.
const FRAME_BYTES: usize = 64 << 10;
/// How many levels [`needs_deep_stack`] goes down: 64 × 64 KiB = 4 MiB.
const DEPTH: usize = 64;
/// The stack of a thread nobody sized: std's default and tokio's (`RUST_MIN_STACK` unset).
const DEFAULT_STACK_BYTES: usize = 2 << 20;
/// The most [`needs_deep_stack`] may use and still leave this much of the 8 MiB to whatever
/// runs it: past it, the calibration is wrong, not the thread.
const MARGIN_BYTES: usize = 2 << 20;

/// One level: `FRAME_BYTES` on this frame, live until the level below returns. Returns the
/// address of the deepest level's buffer.
#[inline(never)]
fn descend(depth: usize) -> usize {
    let mut frame = [0u8; FRAME_BYTES];
    // The address escapes, so the whole buffer is on this frame and is written.
    let frame = black_box(&mut frame);
    frame[depth % FRAME_BYTES] = 1;
    let deepest = if depth == 0 {
        frame.as_ptr() as usize
    } else {
        descend(depth - 1)
    };
    // Read after the call: the buffer is live across it, so the recursion can be neither a
    // loop nor a tail call, and every level keeps its own.
    black_box(&*frame);
    deepest
}

/// Use about `DEPTH × FRAME_BYTES` of the calling thread's stack; returns how many bytes it
/// spanned, from a local of its own to the deepest buffer.
#[inline(never)]
fn needs_deep_stack() -> usize {
    let anchor = 0u8;
    let top = black_box(&anchor) as *const u8 as usize;
    let bottom = descend(black_box(DEPTH));
    top.abs_diff(bottom)
}

/// The span really needed more than a default stack, and leaves the 8 MiB thread its margin.
fn assert_calibrated(span: usize) {
    assert!(
        span > DEFAULT_STACK_BYTES,
        "needs_deep_stack spanned {span} bytes, no more than a default {DEFAULT_STACK_BYTES}: \
         it would pass on an unsized thread, so it proves nothing"
    );
    assert!(
        span <= THREAD_STACK_BYTES - MARGIN_BYTES,
        "needs_deep_stack spanned {span} bytes, too close to the {THREAD_STACK_BYTES} it runs \
         on: recalibrate FRAME_BYTES / DEPTH"
    );
}

#[test]
fn the_blocking_pool_runs_a_deep_core_call() {
    runtime::init_runtime().expect("the runtime builds");
    // What every command does with the core (`commands::blocking`).
    let task = tauri::async_runtime::spawn_blocking(|| {
        let thread = std::thread::current().name().map(str::to_owned);
        (thread, needs_deep_stack())
    });
    let (thread, span) = tauri::async_runtime::block_on(task).expect("the blocking task completes");
    assert_eq!(
        thread.as_deref(),
        Some("tokio-runtime"),
        "it ran on the runtime init_runtime built, not on one Tauri made with default stacks"
    );
    assert_calibrated(span);
}

/// Every event of the operation, into a channel.
struct ToChannel(mpsc::Sender<OpEvent>);

impl EventSink for ToChannel {
    fn send(&self, event: &OpEvent) -> bool {
        self.0.send(event.clone()).is_ok()
    }

    fn webview(&self) -> &str {
        "main"
    }
}

#[test]
fn an_operation_thread_runs_a_deep_core_call() {
    let manager = OperationManager::new(Arc::new(SystemClock));
    let view = manager.register_plan(
        // Bounded: no gesture, so the authenticator that verifies nothing is never asked.
        PlanParts::new(PlanClass::Bounded, "A deep core call", "run"),
        Box::new(|_reporter, _cancel| {
            let thread = std::thread::current().name().map(str::to_owned);
            let span = needs_deep_stack();
            Ok(Outcome::Completed {
                result: json!({ "thread": thread, "span": span }),
            })
        }),
    );
    let (tx, rx) = mpsc::channel();
    manager
        .subscribe(view.op_id, Arc::new(ToChannel(tx)))
        .expect("the plan is there to follow");
    manager
        .execute(view.op_id, &NoAuthenticator)
        .expect("a bounded plan runs without a gesture");

    let result = loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(OpEvent::Finished {
                outcome: Outcome::Completed { result },
            }) => break result,
            Ok(OpEvent::Finished { outcome }) => panic!("the operation ended {outcome:?}"),
            Ok(OpEvent::Failed { error }) => panic!("the operation failed: {error:?}"),
            Ok(_) => {}
            Err(e) => panic!("no final event: {e}"),
        }
    };
    assert_eq!(
        result["thread"],
        json!(format!("op-{}", view.op_id.0)),
        "it ran on the operation's own thread"
    );
    let span = result["span"].as_u64().expect("the span") as usize;
    assert_calibrated(span);
}
