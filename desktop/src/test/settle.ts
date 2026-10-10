// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The teardown step every bun test that mocks IPC runs between cleanup() and clearMocks(): what
// the page already asked of Rust reaches the mock the test installed, never a torn-down one.
//
// Why: a read whose op_start is answered as its component goes still cancels, follows and
// discards its operation (useRead does that, so a late result is released: a verify's draft), on
// microtasks after the unmount. A synchronous teardown cleared the mocks first, and that
// continuation then threw between tests (`transformCallback is not a function`, CI only: there
// the answer arrived later) or, had the next test installed its mock by then, landed in it.
//
// How: one macrotask per turn. The harness (test/ipc.ts) answers on microtasks, op_execute's
// events and the mock engine's (opDelayMs 0) on a 0 ms timer queued by the call itself, so a turn
// in which nothing new was asked means nothing is on its way. A chain of timers that asks nothing
// in between, or a timer longer than 0 ms, is not waited for.

interface Internals {
  invoke?: (...args: unknown[]) => unknown;
}

/** Turns after which IPC that is still being asked is a test that never quiets, not a teardown. */
export const SETTLE_TURNS_MAX = 100;

const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/** Waits until a macrotask turn passes in which nothing new was asked of the IPC mock. */
export async function settleIpc(): Promise<void> {
  const internals = (window as unknown as { __TAURI_INTERNALS__?: Internals }).__TAURI_INTERNALS__;
  const invoke = internals?.invoke;
  if (internals === undefined || invoke === undefined) return;
  const asked: string[] = [];
  const counting = (...args: unknown[]) => {
    asked.push(String(args[0]));
    return invoke.apply(internals, args);
  };
  internals.invoke = counting;
  try {
    let seen = -1;
    for (let turns = 0; seen !== asked.length; turns += 1) {
      if (turns === SETTLE_TURNS_MAX) {
        throw new Error(`IPC still asked after ${turns} turns: ${asked.slice(-5).join(', ')}`);
      }
      seen = asked.length;
      await turn();
    }
  } finally {
    // Unless something installed a mock of its own meanwhile.
    if (internals.invoke === counting) internals.invoke = invoke;
  }
}
