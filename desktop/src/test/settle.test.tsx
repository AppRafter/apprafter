// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, expect, spyOn, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { invoke } from '@tauri-apps/api/core';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, renderHook } from '@testing-library/react';
import * as api from '../ipc/api';
import { resetOperations } from '../ipc/operations';
import { useRead } from '../state/read';
import { completed, installHarness } from './ipc';
import { SETTLE_TURNS_MAX, settleIpc } from './settle';

afterEach(async () => {
  await settleIpc();
  resetOperations();
  clearMocks();
});

test('a read whose op_start is answered after a bare unmount follows and discards its op before the teardown', async () => {
  const h = installHarness();
  h.read('op_start_doctor', [completed({ target: 'prod-eu', groups: [] })]);
  // Rust's follow answers a turn later, as the CI runner's did: one turn is not enough.
  h.answer(
    'op_subscribe',
    () =>
      new Promise((resolve) => {
        setTimeout(() => {
          resolve({ subscription: 1, replay: [completed({ target: 'prod-eu', groups: [] })] });
        }, 0);
      }),
  );
  const { result, unmount } = renderHook(() => useRead<unknown>());
  act(() => {
    void result.current.run(() => api.opStartDoctor('prod-eu'));
  });
  // The component goes, with no lifecycle event (a re-keyed screen, a test's teardown), before
  // op_start's answer reaches the read: nothing is followed yet, and nothing cancels the read.
  unmount();
  expect(h.of('op_subscribe')).toEqual([]);
  await settleIpc();
  const [opId] = h.started('op_start_doctor');
  expect(h.of('op_cancel')).toEqual([]);
  for (const cmd of ['op_subscribe', 'op_discard']) {
    expect({ cmd, args: h.of(cmd).map((c) => c.args.opId) }).toEqual({ cmd, args: [opId] });
  }
});

test('a read whose op_start is answered after the IPC is gone settles, with no unhandled error', async () => {
  const h = installHarness();
  let answer = (_opId: number) => {};
  h.answer(
    'op_start_doctor',
    () =>
      new Promise<number>((resolve) => {
        answer = resolve;
      }),
  );
  const unhandled: unknown[] = [];
  const onUnhandled = (reason: unknown) => unhandled.push(reason);
  process.on('unhandledRejection', onUnhandled);
  // The op_discard that ends the read has no IPC either: logged, as any failed discard is.
  const logged = spyOn(console, 'error').mockImplementation(() => {});
  try {
    const { result, unmount } = renderHook(() => useRead<unknown>());
    let done: Promise<unknown> = Promise.resolve();
    act(() => {
      done = result.current.run(() => api.opStartDoctor('prod-eu'));
    });
    unmount();
    clearMocks();
    answer(h.newOperation([]));
    expect(await done).toBeNull();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(unhandled).toEqual([]);
    expect(logged.mock.calls.map((call) => String(call[0]))).toEqual([
      expect.stringMatching(/^op_discard \d+ failed:$/),
    ]);
  } finally {
    process.off('unhandledRejection', onUnhandled);
    logged.mockRestore();
  }
});

test('a mock asked on every turn fails the teardown with what it was asked, never hangs it', async () => {
  mockIPC(() => null);
  const ping = setInterval(() => {
    void invoke('activity');
  }, 0);
  try {
    await expect(settleIpc()).rejects.toThrow(
      `IPC still asked after ${SETTLE_TURNS_MAX} turns: activity`,
    );
  } finally {
    clearInterval(ping);
  }
});

test('no mock installed: nothing to wait for', async () => {
  clearMocks();
  await settleIpc();
});

/** Every afterEach call of `text` (at the start of a line), as written. */
function afterEachBlocks(text: string): string[] {
  const blocks: string[] = [];
  for (const match of text.matchAll(/^\s*afterEach\(/gm)) {
    let depth = 1;
    let end = match.index + match[0].length;
    while (depth > 0 && end < text.length) {
      if (text[end] === '(') depth += 1;
      else if (text[end] === ')') depth -= 1;
      end += 1;
    }
    blocks.push(text.slice(match.index, end));
  }
  return blocks;
}

function testFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return testFiles(path);
    return /\.test\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

test('every teardown that clears the IPC mock settles it first: after the unmount, before the resets', () => {
  const src = join(import.meta.dir, '..');
  const wrong: string[] = [];
  let checked = 0;
  for (const file of testFiles(src)) {
    for (const block of afterEachBlocks(readFileSync(file, 'utf8'))) {
      const clear = block.indexOf('clearMocks()');
      if (clear === -1) continue;
      checked += 1;
      const settle = block.indexOf('await settleIpc()');
      const cleanup = block.indexOf('cleanup()');
      const reset = block.search(/reset[A-Z]\w*\(\)/);
      if (
        settle === -1 ||
        settle > clear ||
        (cleanup !== -1 && cleanup > settle) ||
        (reset !== -1 && reset < settle)
      ) {
        wrong.push(relative(src, file));
      }
    }
  }
  expect(wrong).toEqual([]);
  // The rule saw the teardowns it is about (this file's own among them).
  expect(checked).toBeGreaterThan(30);
});
