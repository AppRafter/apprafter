// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, jest, test } from 'bun:test';
import type { Channel } from '@tauri-apps/api/core';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, renderHook } from '@testing-library/react';
import { settleIpc } from '../test/settle';
import type { OpEvent } from './generated/OpEvent';
import type { OpSummary } from './generated/OpSummary';
import { useListRefresh } from './listRefresh';
import { attach, refreshList, resetOperations } from './operations';

interface TauriInternals {
  runCallback(id: number, data: unknown): void;
}

let calls: { cmd: string; args: Record<string, unknown> }[];
let summaries: OpSummary[];

beforeEach(() => {
  calls = [];
  summaries = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: args as Record<string, unknown> });
    if (cmd === 'op_list') return summaries;
    if (cmd === 'op_subscribe') return { subscription: 1, replay: [] };
    return null;
  });
});

afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  clearMocks();
});

/**
 * Run `body` with the clock faked: the intervals pass only as the test moves them, so no count
 * depends on how fast the machine ran them (GOTCHA-332). Pending timers run before the real
 * clock is back (a fake 0 ms timer left pending stalls bun's real ones).
 */
async function onFakeTime(body: () => Promise<void>) {
  jest.useFakeTimers();
  try {
    await body();
  } finally {
    act(() => {
      jest.runOnlyPendingTimers();
    });
    jest.useRealTimers();
  }
}

/** `ms` pass on the fake clock; what the reads they start asked has answered. */
const pass = (ms: number) =>
  act(async () => {
    jest.advanceTimersByTime(ms);
    for (let i = 0; i < 20; i += 1) await Promise.resolve();
  });
const lists = () => calls.filter((c) => c.cmd === 'op_list').length;
const summary = (state: OpSummary['state']): OpSummary => ({
  opId: 1,
  title: 'Upgrade platform',
  target: 'prod-eu',
  state,
  startedAtMs: 1,
});

test('idle, nothing is polled', async () => {
  await act(() => refreshList());
  await onFakeTime(async () => {
    renderHook(() => useListRefresh(20));
    await pass(150);
  });
  expect(lists()).toBe(1);
});

test('while an operation runs the list is read again on each interval, and no more once none does', async () => {
  summaries = [summary('running')];
  await act(() => refreshList());
  await onFakeTime(async () => {
    renderHook(() => useListRefresh(20));
    await pass(20);
    await pass(20);
    expect(lists()).toBe(3);
    summaries = [summary('finished')];
    await pass(20);
    expect(lists()).toBe(4);
    await pass(150);
    expect(lists()).toBe(4);
  });
});

test('a followed operation that ends reads the list at once', async () => {
  attach(7);
  await act(() => settleIpc());
  renderHook(() => useListRefresh(60_000));
  await act(() => settleIpc());
  expect(lists()).toBe(0);
  const channel = calls.find((c) => c.cmd === 'op_subscribe')?.args.onEvent as Channel<OpEvent>;
  const internals = (window as unknown as { __TAURI_INTERNALS__: TauriInternals })
    .__TAURI_INTERNALS__;
  await act(async () => {
    internals.runCallback(channel.id, {
      index: 0,
      message: { kind: 'finished', outcome: { status: 'completed', result: null } },
    });
    await settleIpc();
  });
  expect(lists()).toBe(1);
});
