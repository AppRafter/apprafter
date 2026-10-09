// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, spyOn, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, renderHook, waitFor } from '@testing-library/react';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import { resetOperations } from '../ipc/operations';
import { completed, failed, type Harness, installHarness, uiError } from '../test/ipc';
import { useRead } from './read';

let h: Harness;
beforeEach(() => {
  h = installHarness();
});
afterEach(() => {
  resetOperations();
  clearMocks();
});

test('run goes idle → running (no id) → running (id) → done', async () => {
  h.operation(7, [completed({ ok: true })]);
  let release = () => {};
  const started = new Promise<number>((resolve) => {
    release = () => resolve(7);
  });
  const { result } = renderHook(() => useRead<{ ok: boolean }>());
  expect(result.current.state).toEqual({ status: 'idle' });
  let done: Promise<{ ok: boolean } | null> = Promise.resolve(null);
  act(() => {
    done = result.current.run(() => started);
  });
  expect(result.current.state).toEqual({ status: 'running', opId: null });
  await act(async () => {
    release();
    await done;
  });
  expect(result.current.state).toEqual({ status: 'done', data: { ok: true } });
});

test('a failed run ends failed with the UiError', async () => {
  h.operation(8, [failed(uiError('apprafter::target::token_rejected', 'rejected'))]);
  const { result } = renderHook(() => useRead<unknown>());
  await act(async () => {
    await result.current.run(async () => 8);
  });
  expect(result.current.state).toEqual({
    status: 'failed',
    error: uiError('apprafter::target::token_rejected', 'rejected'),
  });
});

test('cancel while running calls op_cancel with the op id', async () => {
  h.operation(9, []); // keeps running (the harness cannot push onto a live subscription)
  const { result } = renderHook(() => useRead<unknown>());
  act(() => {
    void result.current.run(async () => 9);
  });
  await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId: 9 }));
  act(() => result.current.cancel());
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 9 }]);
});

test("a second run cancels the first, and the first one's late end changes nothing", async () => {
  h.operation(10, []);
  h.operation(11, [completed('second')]);
  const { result } = renderHook(() => useRead<string>());
  act(() => {
    void result.current.run(async () => 10);
  });
  await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId: 10 }));
  await act(async () => {
    await result.current.run(async () => 11);
  });
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 10 }]);
  expect(result.current.state).toEqual({ status: 'done', data: 'second' });
});

test('unmount while running calls op_cancel (the close and the lock path)', async () => {
  h.operation(12, []);
  const { result, unmount } = renderHook(() => useRead<unknown>());
  act(() => {
    void result.current.run(async () => 12);
  });
  await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId: 12 }));
  unmount();
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 12 }]);
});

test('an op_cancel refused as locked logs nothing', async () => {
  const spy = spyOn(console, 'error').mockImplementation(() => {});
  h.operation(13, []);
  h.answer('op_cancel', () => Promise.reject(uiError(DESKTOP_ERROR_CODES.LOCKED)));
  const { result, unmount } = renderHook(() => useRead<unknown>());
  act(() => {
    void result.current.run(async () => 13);
  });
  await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId: 13 }));
  unmount();
  await waitFor(() => expect(h.of('op_cancel')).toHaveLength(1));
  expect(spy).not.toHaveBeenCalled();
  spy.mockRestore();
});
