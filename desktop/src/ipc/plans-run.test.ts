// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What Task 10 adds to D.3d's plans.ts, on the IPC harness (D.3d's plans.test.ts runs on the mock
// engine): runRead's onStarted, endOf's refused follow, runPlan, isCancelled, failureOf and
// reportUnlessLocked.
import { afterEach, beforeEach, expect, spyOn, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import {
  cancelled,
  completed,
  failed,
  type Harness,
  installHarness,
  stage,
  uiError,
} from '../test/ipc';
import { IpcError } from './api';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import { operationsSnapshot, resetOperations } from './operations';
import {
  failureOf,
  isCancelled,
  OperationFailed,
  reportUnlessLocked,
  runPlan,
  runRead,
} from './plans';

let h: Harness;
beforeEach(() => {
  h = installHarness();
});
afterEach(() => {
  resetOperations();
  clearMocks();
});

test('runRead hands over the op id as soon as Rust answers, then resolves; the op is forgotten', async () => {
  h.operation(7, [stage(1, 3, 'Target'), completed({ target: 'x', groups: [] })]);
  const seen: number[] = [];
  const report = await runRead<{ target: string; groups: unknown[] }>(
    async () => 7,
    (opId) => seen.push(opId),
  );
  expect(report).toEqual({ target: 'x', groups: [] });
  expect(seen).toEqual([7]);
  expect(h.of('op_discard').map((c) => c.args)).toEqual([{ opId: 7 }]);
  expect(operationsSnapshot().has(7)).toBe(false);
});

test('a failed read is OperationFailed carrying the UiError, and not a cancellation', async () => {
  h.operation(8, [failed(uiError('apprafter::target::token_rejected', 'rejected (HTTP 401)'))]);
  const error = await runRead(async () => 8).catch((e: unknown) => e);
  expect(error).toBeInstanceOf(OperationFailed);
  expect(failureOf(error).code).toBe('apprafter::target::token_rejected');
  expect(isCancelled(error)).toBe(false);
});

test('a cancelled read is isCancelled', async () => {
  h.operation(9, [cancelled()]);
  expect(isCancelled(await runRead(async () => 9).catch((e: unknown) => e))).toBe(true);
});

test('failureOf passes an IpcError through and wraps anything else', () => {
  expect(
    failureOf(new IpcError('op_start_doctor', uiError('apprafter::desktop::locked'))).code,
  ).toBe('apprafter::desktop::locked');
  expect(failureOf(new Error('boom'))).toEqual({
    code: null,
    message: 'boom',
    help: null,
    causes: [],
    fields: {},
  });
});

test('runPlan executes, resolves with the completed result and discards the op', async () => {
  h.operation(10, [completed({ name: 'lab' })]);
  expect(await runPlan<{ name: string }>(10)).toEqual({ name: 'lab' });
  expect(h.of('op_execute')).toHaveLength(1);
  expect(h.of('op_discard').map((c) => c.args)).toEqual([{ opId: 10 }]);
});

test('a follow Rust refuses ends the read with that refusal instead of waiting forever', async () => {
  h.answer('op_subscribe', () => Promise.reject(uiError('apprafter::desktop::plan_not_found')));
  const error = await runRead(async () => 11).catch((e: unknown) => e);
  expect(failureOf(error).code).toBe('apprafter::desktop::plan_not_found');
});

test('reportUnlessLocked stays quiet for the lock refusal only', () => {
  const spy = spyOn(console, 'error').mockImplementation(() => {});
  reportUnlessLocked('op_cancel 7')(new IpcError('op_cancel', uiError(DESKTOP_ERROR_CODES.LOCKED)));
  expect(spy).not.toHaveBeenCalled();
  reportUnlessLocked('op_cancel 7')(new Error('boom'));
  expect(spy).toHaveBeenCalledTimes(1);
  spy.mockRestore();
});
