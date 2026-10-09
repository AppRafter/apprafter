// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A plan and a read followed to their ends, on the mock IPC and the real operations store.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import * as api from './api';
import { CORE_ERROR_CODES } from './generated/core-errors';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { WhoamiReport } from './generated/WhoamiReport';
import { installMockIpc, mockOps } from './mock';
import { resetOperations } from './operations';
import { OperationFailed, resultOf, runRead, startPlan } from './plans';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(() => {
  resetOperations();
  clearMocks();
});

/** The discard's IPC round trip. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test('startPlan runs a plan; its end resolves with the outcome, and the op is discarded', async () => {
  const view = await api.opPlanTargetUse('staging');
  const run = await startPlan(view.opId);
  expect(resultOf(await run.ended)).toMatchObject({
    name: 'staging',
    pointer: { from: 'prod-eu', to: 'staging' },
  });
  await settle();
  expect((await api.opList()).some((s) => s.opId === view.opId)).toBe(false);
});

test('a refused execute rejects startPlan; a failed run is OperationFailed from resultOf', async () => {
  const gone = await startPlan(999).catch((e: unknown) => e);
  expect((gone as api.IpcError).error.code).toBe(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND);
  const taken = {
    code: CORE_ERROR_CODES.TARGET_EXISTS,
    message: 'taken',
    help: null,
    causes: [],
    fields: {},
  };
  const failing = mockOps().registerPlan(
    { class: 'bounded', title: 't', changes: [], target: null },
    { end: () => ({ error: taken }) },
  );
  const end = await (await startPlan(failing.opId)).ended;
  expect(() => resultOf(end)).toThrow(OperationFailed);
  try {
    resultOf(end);
  } catch (e) {
    expect((e as OperationFailed).error).toEqual(taken);
  }
});

test('a cancelled run is OperationFailed with OP_CANCELLED', async () => {
  // Slow enough that the cancel comes first, whatever the timers do.
  clearMocks();
  installMockIpc({ opDelayMs: 10_000 });
  await api.unlock();
  const view = mockOps().registerPlan(
    { class: 'bounded', title: 't', changes: [], target: null },
    { end: () => ({ result: null }) },
  );
  const run = await startPlan(view.opId);
  await api.opCancel(view.opId);
  const end = await run.ended;
  expect(end.state).toBe('cancelled');
  expect(() => resultOf(end)).toThrow(OperationFailed);
  try {
    resultOf(end);
  } catch (e) {
    expect((e as OperationFailed).error.code).toBe(CORE_ERROR_CODES.OP_CANCELLED);
  }
});

test('runRead follows a read to its result, and discards it', async () => {
  const report = await runRead<WhoamiReport>(() => api.opStartWhoami());
  expect(report.identity).toBe('anonymous_self_hosted');
  await settle();
  expect(await api.opList()).toEqual([]);
});

test('runRead rejects with OperationFailed when the read fails', async () => {
  const failed = await runRead(() => api.opStartVerifyToken('hetzner-cloud', 'x'.repeat(64))).catch(
    (e: unknown) => e,
  );
  expect(failed).toBeInstanceOf(OperationFailed);
  expect((failed as OperationFailed).error.code).toBe(CORE_ERROR_CODES.TARGET_TOKEN_REJECTED);
});
