// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import type { Channel } from '@tauri-apps/api/core';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, renderHook } from '@testing-library/react';
import { IpcError } from './api';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { OpEvent } from './generated/OpEvent';
import type { OpSummary } from './generated/OpSummary';
import type { Outcome } from './generated/Outcome';
import type { Subscribed } from './generated/Subscribed';
import type { JsonValue } from './generated/serde_json/JsonValue';
import type { UiError } from './generated/UiError';
import {
  attach,
  cancel,
  clearLive,
  discard,
  execute,
  OUTPUT_CAP,
  operationsSnapshot,
  reattachAll,
  refreshList,
  resetOperations,
  useOperation,
} from './operations';

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason: unknown) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const uiError = (code: string, message = code): UiError => ({
  code,
  message,
  help: null,
  causes: [],
  fields: {},
});

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

let calls: Call[];
/** Per command, what the next call answers (a value, or a promise the test settles). */
let answers: Map<string, unknown[]>;

function answerNext(cmd: string, value: unknown) {
  answers.set(cmd, [...(answers.get(cmd) ?? []), value]);
}

beforeEach(() => {
  calls = [];
  answers = new Map();
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: args as Record<string, unknown> });
    const queue = answers.get(cmd) ?? [];
    return queue.length > 0 ? queue.shift() : null;
  });
});

afterEach(() => {
  resetOperations();
  clearMocks();
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** The channel the n-th call of `cmd` carried. */
function channelOf(cmd: string, n = 0): Channel<OpEvent> {
  const call = calls.filter((c) => c.cmd === cmd)[n];
  if (call === undefined) throw new Error(`no call ${n} of ${cmd}`);
  return call.args.onEvent as Channel<OpEvent>;
}

/** Rust's side of a channel: message `index`, in whatever order the test sends them. */
function send(channel: Channel<OpEvent>, index: number, message: OpEvent) {
  const internals = (window as unknown as { __TAURI_INTERNALS__: TauriInternals })
    .__TAURI_INTERNALS__;
  internals.runCallback(channel.id, { index, message });
}

interface TauriInternals {
  runCallback(id: number, data: unknown): void;
}

const out = (text: string): OpEvent => ({ kind: 'output', stream: 'stdout', text });
const view = (opId: number) => operationsSnapshot().get(opId);
const texts = (opId: number) => view(opId)?.lines.map((l) => l.text);
const subscribed = (subscription: number, replay: OpEvent[] = []): Subscribed => ({
  subscription,
  replay,
});

describe('attach', () => {
  test('applies the snapshot, then the live events', async () => {
    answerNext(
      'op_subscribe',
      subscribed(1, [{ kind: 'stage', index: 1, total: 3, title: 'Plan' }, out('a')]),
    );
    attach(7);
    await settle();
    send(channelOf('op_subscribe'), 0, out('b'));
    expect(texts(7)).toEqual(['a', 'b']);
    expect(view(7)?.stage).toEqual({ index: 1, total: 3, title: 'Plan' });
    expect(view(7)?.live).toBe(true);
  });

  test('a live event that reaches JS before the snapshot waits for it', async () => {
    const answer = deferred<Subscribed>();
    answerNext('op_subscribe', answer.promise);
    attach(7);
    send(channelOf('op_subscribe'), 0, out('b'));
    expect(texts(7)).toEqual([]);
    answer.resolve(subscribed(1, [out('a')]));
    await settle();
    expect(texts(7)).toEqual(['a', 'b']);
  });

  test('channel messages that arrive out of order are applied in order', async () => {
    answerNext('op_subscribe', subscribed(1));
    attach(7);
    await settle();
    const channel = channelOf('op_subscribe');
    send(channel, 2, out('c'));
    send(channel, 0, out('a'));
    send(channel, 1, out('b'));
    expect(texts(7)).toEqual(['a', 'b', 'c']);
  });

  test('Finished marks the operation done, and its summary', async () => {
    answerNext('op_list', [summary(7, 'running')]);
    await refreshList();
    answerNext('op_subscribe', subscribed(1));
    attach(7);
    await settle();
    send(channelOf('op_subscribe'), 0, {
      kind: 'finished',
      outcome: { status: 'completed', result: 42 },
    });
    expect(view(7)?.end).toEqual({
      state: 'finished',
      outcome: { status: 'completed', result: 42 },
    });
    expect(view(7)?.summary?.state).toBe('finished');
  });

  test('a cancelled outcome ends as cancelled, a Failed as failed', async () => {
    answerNext('op_subscribe', subscribed(1));
    answerNext('op_subscribe', subscribed(2));
    attach(7);
    attach(8);
    await settle();
    const cancelled: Outcome<JsonValue> = { status: 'cancelled', cleaned: ['vm'], left: [] };
    send(channelOf('op_subscribe', 0), 0, { kind: 'finished', outcome: cancelled });
    const failed = uiError('apprafter::io::error', 'disk full');
    send(channelOf('op_subscribe', 1), 0, { kind: 'failed', error: failed });
    expect(view(7)?.end).toEqual({ state: 'cancelled', outcome: cancelled });
    expect(view(8)?.end).toEqual({ state: 'failed', error: failed });
  });

  test('OutputDropped becomes a visible line', async () => {
    answerNext('op_subscribe', subscribed(1, [{ kind: 'output_dropped', bytes: 2048 }, out('x')]));
    attach(7);
    await settle();
    expect(view(7)?.lines).toEqual([
      { kind: 'dropped', bytes: 2048, text: 'Earlier output was dropped (2048 bytes).' },
      { kind: 'stdout', text: 'x' },
    ]);
  });

  test('warnings and notices are lines too, in arrival order', async () => {
    answerNext('op_subscribe', subscribed(1));
    attach(7);
    await settle();
    const channel = channelOf('op_subscribe');
    send(channel, 0, out('a'));
    send(channel, 1, { kind: 'warning', message: 'slow' });
    send(channel, 2, { kind: 'output', stream: 'stderr', text: 'e' });
    send(channel, 3, { kind: 'notice', message: 'note' });
    send(channel, 4, { kind: 'progress', done: 3, total: null, unit: 'MiB' });
    expect(view(7)?.lines.map((l) => l.kind)).toEqual(['stdout', 'warning', 'stderr', 'notice']);
    expect(view(7)?.progress).toEqual({ done: 3, total: null, unit: 'MiB' });
  });

  test('output past the cap drops the oldest output, counted in one leading line', async () => {
    answerNext('op_subscribe', subscribed(1, [{ kind: 'output_dropped', bytes: 10 }]));
    attach(7);
    await settle();
    const chunk = 'x'.repeat(8 * 1024);
    const chunks = OUTPUT_CAP / chunk.length + 2;
    const channel = channelOf('op_subscribe');
    for (let i = 0; i < chunks; i++) send(channel, i, out(chunk));
    const lines = view(7)?.lines ?? [];
    expect(lines[0]).toEqual({
      kind: 'dropped',
      bytes: 10 + 2 * chunk.length,
      text: `Earlier output was dropped (${10 + 2 * chunk.length} bytes).`,
    });
    expect(lines.slice(1).every((l) => l.kind === 'stdout')).toBe(true);
    expect(lines.length - 1).toBe(OUTPUT_CAP / chunk.length);
  });

  test('a refusal other than the lock is recorded on the view', async () => {
    const gone = uiError(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND);
    answerNext('op_subscribe', Promise.reject(gone));
    attach(7);
    await settle();
    expect(view(7)?.attachError).toEqual(gone);
    expect(view(7)?.live).toBe(false);
  });
});

describe('followers', () => {
  test('one subscription however many follow; the last release ends it', async () => {
    answerNext('op_subscribe', subscribed(4));
    const first = attach(7);
    const second = attach(7);
    await settle();
    expect(calls.filter((c) => c.cmd === 'op_subscribe')).toHaveLength(1);
    first();
    first();
    expect(calls.filter((c) => c.cmd === 'op_unsubscribe')).toHaveLength(0);
    second();
    await settle();
    expect(calls.filter((c) => c.cmd === 'op_unsubscribe')).toEqual([
      { cmd: 'op_unsubscribe', args: { opId: 7, subscription: 4 } },
    ]);
    expect(view(7)).toBeUndefined();
  });

  test('released before its snapshot arrived, the subscription is ended once it does', async () => {
    const answer = deferred<Subscribed>();
    answerNext('op_subscribe', answer.promise);
    const release = attach(7);
    release();
    answer.resolve(subscribed(5, [out('a')]));
    await settle();
    expect(calls.filter((c) => c.cmd === 'op_unsubscribe')).toEqual([
      { cmd: 'op_unsubscribe', args: { opId: 7, subscription: 5 } },
    ]);
    expect(view(7)).toBeUndefined();
  });

  test('an unsubscribe refused because the app locked is expected; anything else is reported', async () => {
    const reported = spyOn(console, 'error').mockImplementation(() => {});
    try {
      answerNext('op_subscribe', subscribed(1));
      answerNext('op_subscribe', subscribed(2));
      const locked = attach(7);
      const broken = attach(8);
      await settle();
      answerNext('op_unsubscribe', Promise.reject(uiError(DESKTOP_ERROR_CODES.LOCKED)));
      locked();
      await settle();
      expect(reported).not.toHaveBeenCalled();
      answerNext('op_unsubscribe', Promise.reject(uiError(DESKTOP_ERROR_CODES.INTERNAL)));
      broken();
      await settle();
      expect(reported).toHaveBeenCalledTimes(1);
    } finally {
      reported.mockRestore();
    }
  });
});

describe('the lock', () => {
  test('re-attach after a cleared store restores the state from the replay', async () => {
    answerNext('op_subscribe', subscribed(1, [out('a')]));
    attach(7);
    await settle();
    const before = channelOf('op_subscribe', 0);
    send(before, 0, out('b'));

    clearLive();
    expect(texts(7)).toEqual([]);
    expect(view(7)?.live).toBe(false);
    // Rust ended that subscription; anything still on its way is not applied.
    send(before, 1, out('stale'));
    expect(texts(7)).toEqual([]);

    answerNext('op_subscribe', subscribed(2, [out('a'), out('b'), out('c')]));
    reattachAll();
    await settle();
    expect(texts(7)).toEqual(['a', 'b', 'c']);
    expect(view(7)?.live).toBe(true);
    // Rust dropped the old subscription itself; nobody asks it to.
    expect(calls.filter((c) => c.cmd === 'op_unsubscribe')).toHaveLength(0);
  });

  test('an attach refused while locked is attached again by reattachAll, with no error', async () => {
    answerNext('op_subscribe', Promise.reject(uiError(DESKTOP_ERROR_CODES.LOCKED)));
    attach(7);
    await settle();
    expect(view(7)?.attachError).toBeNull();
    answerNext('op_subscribe', subscribed(3, [out('a')]));
    reattachAll();
    await settle();
    expect(texts(7)).toEqual(['a']);
  });

  test('reattachAll leaves an operation nobody follows alone', async () => {
    answerNext('op_list', [summary(7, 'running')]);
    await refreshList();
    reattachAll();
    await settle();
    expect(calls.filter((c) => c.cmd === 'op_subscribe')).toHaveLength(0);
  });
});

describe('execute', () => {
  test('follows the operation from its first event, which may arrive before the answer', async () => {
    const answer = deferred<number>();
    answerNext('op_execute', answer.promise);
    const running = execute(5);
    send(channelOf('op_execute'), 0, { kind: 'stage', index: 1, total: 2, title: 'Create' });
    answer.resolve(9);
    const release = await running;
    send(channelOf('op_execute'), 1, out('a'));
    expect(view(5)?.stage?.title).toBe('Create');
    expect(texts(5)).toEqual(['a']);
    release();
    await settle();
    expect(calls.filter((c) => c.cmd === 'op_unsubscribe')).toEqual([
      { cmd: 'op_unsubscribe', args: { opId: 5, subscription: 9 } },
    ]);
  });

  test('a rejection is the answer: it throws, and the Failed on its own channel is not shown', async () => {
    const expired = uiError(DESKTOP_ERROR_CODES.PLAN_EXPIRED, 'The plan expired.');
    const answer = deferred<number>();
    answerNext('op_execute', answer.promise);
    const running = execute(5);
    send(channelOf('op_execute'), 0, { kind: 'failed', error: expired });
    answer.reject(expired);
    const thrown = await running.catch((e: unknown) => e);
    expect(thrown).toBeInstanceOf(IpcError);
    expect((thrown as IpcError).error).toEqual(expired);
    expect(view(5)).toBeUndefined();
  });

  test('a rejected execute leaves an earlier subscription following the plan', async () => {
    // A busy prompt: Rust sends nothing, the plan waits, and this call's subscription is gone.
    answerNext('op_subscribe', subscribed(1));
    attach(5);
    await settle();
    answerNext('op_execute', Promise.reject(uiError(DESKTOP_ERROR_CODES.AUTH_BUSY)));
    await expect(execute(5)).rejects.toBeInstanceOf(IpcError);
    send(channelOf('op_subscribe'), 0, { kind: 'stage', index: 1, total: 1, title: 'Run' });
    expect(view(5)?.stage?.title).toBe('Run');
    expect(view(5)?.live).toBe(true);
    expect(calls.filter((c) => c.cmd === 'op_unsubscribe')).toHaveLength(0);
  });

  test('an earlier subscription to the plan ends once execute answers; nothing shows twice', async () => {
    answerNext('op_subscribe', subscribed(1));
    attach(5);
    await settle();
    const answer = deferred<number>();
    answerNext('op_execute', answer.promise);
    const running = execute(5);
    // Both channels carry the operation's events from its start.
    send(channelOf('op_subscribe'), 0, out('a'));
    send(channelOf('op_execute'), 0, out('a'));
    answer.resolve(2);
    await running;
    send(channelOf('op_subscribe'), 1, out('b'));
    send(channelOf('op_execute'), 1, out('b'));
    expect(texts(5)).toEqual(['a', 'b']);
    await settle();
    expect(calls.filter((c) => c.cmd === 'op_unsubscribe')).toEqual([
      { cmd: 'op_unsubscribe', args: { opId: 5, subscription: 1 } },
    ]);
  });
});

function summary(opId: number, state: OpSummary['state']): OpSummary {
  return { opId, title: `op ${opId}`, target: 'prod', state, startedAtMs: opId };
}

describe('the list, cancel and discard', () => {
  test('refreshList keeps the listed operations and forgets an unlisted one nobody follows', async () => {
    answerNext('op_list', [summary(7, 'running'), summary(8, 'finished')]);
    await refreshList();
    expect([...operationsSnapshot().keys()].sort()).toEqual([7, 8]);
    answerNext('op_list', [summary(7, 'finished')]);
    await refreshList();
    expect([...operationsSnapshot().keys()]).toEqual([7]);
    expect(view(7)?.summary?.state).toBe('finished');
  });

  test('cancel invokes op_cancel', async () => {
    await cancel(7);
    expect(calls).toEqual([{ cmd: 'op_cancel', args: { opId: 7 } }]);
  });

  test('discard invokes op_discard and forgets the operation', async () => {
    answerNext('op_list', [summary(7, 'finished')]);
    await refreshList();
    await discard(7);
    expect(calls.at(-1)).toEqual({ cmd: 'op_discard', args: { opId: 7 } });
    expect(view(7)).toBeUndefined();
  });
});

test('useOperation re-renders on each event and keeps its snapshot between them', async () => {
  answerNext('op_subscribe', subscribed(1));
  attach(7);
  await settle();
  const { result } = renderHook(() => useOperation(7));
  const first = result.current;
  expect(first?.lines).toEqual([]);
  act(() => send(channelOf('op_subscribe'), 0, out('a')));
  expect(result.current?.lines.map((l) => l.text)).toEqual(['a']);
  expect(operationsSnapshot()).toBe(operationsSnapshot());
});
