// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The lock state cannot miss a change, nor go back to an older one: the `lock-changed`
// listener is in place before `lock_status` is asked (the listener that stays, under
// StrictMode's double mount too), every registration is followed by a read, and wherever the
// ['lock'] entry is written — the read, an event, the answer of lock_now or unlock — a state
// of an earlier transition (`seq`) than the one held is dropped, whatever its time says.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { type QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, renderHook } from '@testing-library/react';
import type { ReactNode } from 'react';
import { LOCK_CHANGED } from '../ipc/generated/events';
import type { LockState } from '../ipc/generated/LockState';
import { lockState } from '../test/fixtures';
import { settleIpc } from '../test/settle';
import { useLockActions, useLockState } from './lock';
import { createQueryClient } from './queryClient';

interface TauriInternals {
  runCallback(id: number, data: unknown): void;
}

interface Held<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
}

function held<T>(): Held<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => (resolve = res));
  return { promise, resolve };
}

let order: string[];
let handler: number | undefined;
/** Each `lock_status` waits for the test; `statuses` holds them in the order asked. */
let statuses: Held<LockState>[];
/** Off: each listen registers at once. On: each waits in `listens` for the test. */
let holdListens: boolean;
let listens: Held<void>[];
let answers: Record<'lock_now' | 'unlock', Held<LockState> | undefined>;

beforeEach(() => {
  order = [];
  handler = undefined;
  statuses = [];
  holdListens = false;
  listens = [];
  answers = { lock_now: undefined, unlock: undefined };
  // Events mocked by hand: the test sees when a listener registers and delivers to it.
  mockIPC(async (cmd, args) => {
    order.push(cmd);
    if (cmd === 'plugin:event|listen') {
      const id = (args as { handler: number }).handler;
      if (holdListens) {
        const registered = held<void>();
        listens.push(registered);
        await registered.promise;
      }
      handler = id;
      order.push('listening');
      return id;
    }
    if (cmd === 'lock_status') {
      const status = held<LockState>();
      statuses.push(status);
      return status.promise;
    }
    if (cmd === 'lock_now' || cmd === 'unlock') {
      const answer = held<LockState>();
      answers[cmd] = answer;
      return answer.promise;
    }
    return null;
  });
});

afterEach(async () => {
  cleanup();
  await settleIpc();
  clearMocks();
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A `lock-changed` event, and the turn the query cache takes to tell its observers. */
async function deliver(state: LockState) {
  if (handler === undefined) throw new Error('no lock-changed listener');
  const id = handler;
  const internals = (window as unknown as { __TAURI_INTERNALS__: TauriInternals })
    .__TAURI_INTERNALS__;
  await act(async () => {
    internals.runCallback(id, { event: LOCK_CHANGED, id: 1, payload: state });
    await settle();
  });
}

function mount(client: QueryClient = createQueryClient(), strict = false) {
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  // RTL's own option: a <StrictMode> in the wrapper does not double the hook's effects.
  return renderHook(() => ({ state: useLockState(), actions: useLockActions() }), {
    wrapper,
    reactStrictMode: strict,
  });
}

async function answerStatus(state: LockState) {
  await act(async () => {
    statuses.at(-1)?.resolve(state);
    await settle();
  });
}

test('the listener is registered before lock_status is asked', async () => {
  mount();
  await act(settle);
  expect(order.slice(0, 3)).toEqual(['plugin:event|listen', 'listening', 'lock_status']);
});

test('an event during the startup read wins over an older answer', async () => {
  const { result } = mount();
  await act(settle);
  await deliver(lockState({ locked: false, sinceMs: 2_000, seq: 2 }));
  await answerStatus(lockState({ locked: true, reason: 'startup', sinceMs: 1_000, seq: 1 }));
  expect(result.current.state.data).toEqual(lockState({ locked: false, sinceMs: 2_000, seq: 2 }));
});

test('an answer newer than an earlier event wins', async () => {
  const { result } = mount();
  await act(settle);
  await deliver(lockState({ locked: false, sinceMs: 1_000, seq: 1 }));
  await answerStatus(lockState({ locked: true, reason: 'idle', sinceMs: 3_000, seq: 3 }));
  expect(result.current.state.data).toEqual(
    lockState({ locked: true, reason: 'idle', sinceMs: 3_000, seq: 3 }),
  );
});

test('an event older than the state held is dropped', async () => {
  const { result } = mount();
  await act(settle);
  await answerStatus(lockState({ locked: true, reason: 'idle', sinceMs: 3_000, seq: 3 }));
  await deliver(lockState({ locked: false, sinceMs: 2_000, seq: 2 }));
  expect(result.current.state.data?.sinceMs).toBe(3_000);
  expect(result.current.state.data?.locked).toBe(true);
  // A newer one is taken.
  await deliver(lockState({ locked: false, sinceMs: 4_000, seq: 4 }));
  expect(result.current.state.data?.locked).toBe(false);
});

test('an unlock answer that arrives after a newer lock is dropped', async () => {
  const { result } = mount();
  await act(settle);
  await answerStatus(lockState({ locked: true, reason: 'startup', sinceMs: 1_000, seq: 1 }));
  let unlocking: Promise<void> | undefined;
  act(() => {
    unlocking = result.current.actions.unlock();
  });
  // The unlock happens (its event is late), then the idle timer locks again: that event
  // overtakes the unlock's answer.
  await deliver(lockState({ locked: true, reason: 'idle', sinceMs: 3_000, seq: 3 }));
  await act(async () => {
    answers.unlock?.resolve(lockState({ locked: false, sinceMs: 2_000, seq: 2 }));
    await unlocking;
    await settle();
  });
  expect(result.current.state.data).toEqual(
    lockState({ locked: true, reason: 'idle', sinceMs: 3_000, seq: 3 }),
  );
});

test('a lock whose time is earlier than the state held still locks: the clock stepped back', async () => {
  const { result } = mount();
  await act(settle);
  await answerStatus(lockState({ locked: false, sinceMs: 5_000, seq: 1 }));
  // The wall clock went back past the idle time, and the idle lock came after the unlock.
  const idle = lockState({ locked: true, reason: 'idle', sinceMs: 1_000, seq: 2 });
  await deliver(idle);
  expect(result.current.state.data).toEqual(idle);
  // So does Lock now, then: its answer is taken too.
  let locking: Promise<void> | undefined;
  act(() => {
    locking = result.current.actions.lock();
  });
  await act(async () => {
    answers.lock_now?.resolve(idle);
    await locking;
    await settle();
  });
  expect(result.current.state.data).toEqual(idle);
});

test('an unlock and a lock within one millisecond end locked, whatever arrives last', async () => {
  const { result } = mount();
  await act(settle);
  await answerStatus(lockState({ locked: true, reason: 'startup', sinceMs: 1_000, seq: 0 }));
  let unlocking: Promise<void> | undefined;
  act(() => {
    unlocking = result.current.actions.unlock();
  });
  // The unlock, then an OS session lock from another thread, in the same millisecond; the
  // unlock's answer comes last.
  const unlocked = lockState({ locked: false, sinceMs: 7_000, seq: 1 });
  const osSession = lockState({ locked: true, reason: 'os_session', sinceMs: 7_000, seq: 2 });
  await deliver(unlocked);
  await deliver(osSession);
  await act(async () => {
    answers.unlock?.resolve(unlocked);
    await unlocking;
    await settle();
  });
  expect(result.current.state.data).toEqual(osSession);
});

test('a lock answer is taken over an older state, and its own event changes nothing', async () => {
  const { result } = mount();
  await act(settle);
  await answerStatus(lockState({ locked: false, sinceMs: 1_000, seq: 1 }));
  let locking: Promise<void> | undefined;
  act(() => {
    locking = result.current.actions.lock();
  });
  const manual = lockState({ locked: true, reason: 'manual', sinceMs: 2_000, seq: 2 });
  await act(async () => {
    answers.lock_now?.resolve(manual);
    await locking;
    await settle();
  });
  expect(result.current.state.data).toEqual(manual);
  await deliver(manual);
  expect(result.current.state.data).toEqual(manual);
});

test('under StrictMode the read waits for the listener that stays, not the one dropped', async () => {
  holdListens = true;
  mount(createQueryClient(), true);
  await act(settle);
  expect(listens).toHaveLength(2);
  // The first mount's registration lands after its cleanup: it is unlistened at once.
  await act(async () => {
    listens[0]?.resolve();
    await settle();
  });
  expect(order).not.toContain('lock_status');
  await act(async () => {
    listens[1]?.resolve();
    await settle();
  });
  expect(order.filter((cmd) => cmd === 'lock_status')).toHaveLength(1);
});

test('a listener registered again is followed by a read: what changed meanwhile is not missed', async () => {
  const client = createQueryClient();
  const first = mount(client);
  await act(settle);
  await answerStatus(lockState({ locked: false, sinceMs: 1_000, seq: 1 }));
  first.unmount();
  // While nothing listens, the app locks.
  const { result } = mount(client);
  await act(settle);
  expect(order.filter((cmd) => cmd === 'lock_status')).toHaveLength(2);
  await answerStatus(lockState({ locked: true, reason: 'idle', sinceMs: 2_000, seq: 2 }));
  expect(result.current.state.data?.locked).toBe(true);
});
