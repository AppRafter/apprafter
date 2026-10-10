// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import type { Channel } from '@tauri-apps/api/core';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, renderHook } from '@testing-library/react';
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

afterEach(() => {
  cleanup();
  resetOperations();
  clearMocks();
});

const wait = (ms: number) => act(() => new Promise((resolve) => setTimeout(resolve, ms)));
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
  renderHook(() => useListRefresh(20));
  await wait(150);
  expect(lists()).toBe(1);
});

test('while an operation runs the list is read again on each interval, and no more once none does', async () => {
  summaries = [summary('running')];
  await act(() => refreshList());
  renderHook(() => useListRefresh(20));
  await wait(90);
  expect(lists()).toBeGreaterThanOrEqual(3);
  summaries = [summary('finished')];
  await wait(60);
  const settled = lists();
  await wait(150);
  expect(lists()).toBe(settled);
});

test('a followed operation that ends reads the list at once', async () => {
  attach(7);
  await wait(0);
  renderHook(() => useListRefresh(60_000));
  await wait(20);
  expect(lists()).toBe(0);
  const channel = calls.find((c) => c.cmd === 'op_subscribe')?.args.onEvent as Channel<OpEvent>;
  const internals = (window as unknown as { __TAURI_INTERNALS__: TauriInternals })
    .__TAURI_INTERNALS__;
  await act(async () => {
    internals.runCallback(channel.id, {
      index: 0,
      message: { kind: 'finished', outcome: { status: 'completed', result: null } },
    });
    await new Promise((resolve) => setTimeout(resolve, 10));
  });
  expect(lists()).toBe(1);
});
