// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The mock operation engine, through the real operations store and api.ts: plans that run once,
// reads that start at once, cancel, discard, op_list, and Rust's lock hook.
import { afterEach, beforeEach, expect, mock, setSystemTime, test } from 'bun:test';
import { Channel } from '@tauri-apps/api/core';
import { clearMocks } from '@tauri-apps/api/mocks';
import * as api from '../api';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import type { OpEvent } from '../generated/OpEvent';
import type { UiError } from '../generated/UiError';
import { attach, execute, operationsSnapshot, resetOperations } from '../operations';
import { installMockIpc, MOCK_PASSWORD, mockOps } from './index';
import { MOCK_PLAN_TTL_MS } from './ops';

const settle = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms));
async function endOfView(opId: number) {
  for (let i = 0; i < 200 && (operationsSnapshot().get(opId)?.end ?? null) === null; i += 1) {
    await settle(2);
  }
  return operationsSnapshot().get(opId)?.end;
}
/** The UiError code a call rejected with. */
const codeOf = (e: unknown) => (e as api.IpcError).error.code;
const taken: UiError = {
  code: 'apprafter::target::exists',
  message: 'target `lab` already exists',
  help: null,
  causes: [],
  fields: {},
};

beforeEach(() => resetOperations());
afterEach(() => {
  setSystemTime();
  clearMocks();
});

test('a registered plan runs once on op_execute and its result reaches the store', async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
  const view = mockOps().registerPlan(
    { class: 'bounded', title: 'Rename prod', changes: [], target: 'prod' },
    {
      events: [{ kind: 'notice', message: 'renamed' }],
      end: () => ({ result: { from: 'prod', to: 'eu' } }),
    },
  );
  const release = await execute(view.opId);
  expect(await endOfView(view.opId)).toEqual({
    state: 'finished',
    outcome: { status: 'completed', result: { from: 'prod', to: 'eu' } },
  });
  expect(
    operationsSnapshot()
      .get(view.opId)
      ?.lines.map((l) => l.text),
  ).toEqual(['renamed']);
  release();
  const again = await api.opExecute(view.opId, new Channel<OpEvent>()).catch((e: unknown) => e);
  expect(codeOf(again)).toBe(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND);
});

test('a destructive plan asks the gesture: on the PAM route a wrong password keeps the plan', async () => {
  installMockIpc({ opDelayMs: 0, auth: 'pam' });
  await api.unlockWithPassword(MOCK_PASSWORD);
  const view = mockOps().registerPlan(
    { class: 'destructive', title: 'Remove prod', changes: [], target: 'prod' },
    { end: () => ({ result: null }) },
  );
  const refused = await execute(view.opId, 'guess').catch((e: unknown) => e);
  expect(codeOf(refused)).toBe(DESKTOP_ERROR_CODES.AUTH_FAILED);
  await execute(view.opId, MOCK_PASSWORD);
  expect((await endOfView(view.opId))?.state).toBe('finished');
});

test('where the OS prompts, the prompt is the gesture; a password there is refused and keeps the plan', async () => {
  installMockIpc({ opDelayMs: 0, os: 'linux' });
  await api.unlock();
  const view = mockOps().registerPlan(
    { class: 'destructive', title: 'Remove prod', changes: [], target: 'prod' },
    { end: () => ({ result: null }) },
  );
  const refused = await execute(view.opId, MOCK_PASSWORD).catch((e: unknown) => e);
  expect((refused as api.IpcError).error).toMatchObject({
    code: DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
    fields: { reason: 'use_system_prompt' },
  });
  await execute(view.opId);
  expect((await endOfView(view.opId))?.state).toBe('finished');
});

test('a read starts at once, is followed through op_subscribe, and op_cancel ends it cancelled', async () => {
  installMockIpc({ opDelayMs: 50 });
  await api.unlock();
  const id = mockOps().startRead('Doctor · prod', 'prod', {
    end: () => ({ result: { groups: [] } }),
  });
  expect((await api.opList()).find((s) => s.opId === id)).toMatchObject({
    title: 'Doctor · prod',
    target: 'prod',
    state: 'running',
  });
  const release = attach(id);
  await api.opCancel(id);
  expect((await endOfView(id))?.state).toBe('cancelled');
  release();
  expect((await api.opList()).find((s) => s.opId === id)?.state).toBe('cancelled');
  await api.opCancel(id); // an ended operation is left as it is
  expect((await api.opList()).find((s) => s.opId === id)?.state).toBe('cancelled');
});

test('a lock drops the plans and cancels the reads, as Rust does', async () => {
  installMockIpc({ opDelayMs: 50 });
  await api.unlock();
  const plan = mockOps().registerPlan(
    { class: 'bounded', title: 'x', changes: [], target: null },
    { end: () => ({ result: null }) },
  );
  const heard: OpEvent[] = [];
  await api.opSubscribe(plan.opId, new Channel<OpEvent>((event) => heard.push(event)));
  const read = mockOps().startRead('Doctor', null, { end: () => ({ result: null }) });
  await api.lockNow();
  expect(heard).toEqual([
    { kind: 'failed', error: expect.objectContaining({ code: DESKTOP_ERROR_CODES.LOCKED }) },
  ]);
  await api.unlock();
  const gone = await execute(plan.opId).catch((e: unknown) => e);
  expect(codeOf(gone)).toBe(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND);
  expect((await api.opList()).find((s) => s.opId === read)?.state).toBe('cancelled');
});

test('a page that followed the plan follows its run; the run is asked how it ends only then', async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
  const end = mock(() => ({ result: 1 }));
  const view = mockOps().registerPlan(
    { class: 'bounded', title: 'Use lab', changes: [], target: 'lab' },
    { events: [{ kind: 'notice', message: 'one' }], end },
  );
  const release = attach(view.opId);
  await settle();
  expect(operationsSnapshot().get(view.opId)?.live).toBe(true);
  await api.opExecute(view.opId, new Channel<OpEvent>());
  expect(end).not.toHaveBeenCalled();
  expect((await endOfView(view.opId))?.state).toBe('finished');
  expect(end).toHaveBeenCalledTimes(1);
  expect(
    operationsSnapshot()
      .get(view.opId)
      ?.lines.map((l) => l.text),
  ).toEqual(['one']);
  release();
  const late = await api.opSubscribe(view.opId, new Channel<OpEvent>());
  expect(late.replay.map((e) => e.kind)).toEqual(['notice', 'finished']);
});

test('cancelling a plan drops it and tells nobody; discard forgets an ended operation only', async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
  const plan = mockOps().registerPlan(
    { class: 'bounded', title: 'x', changes: [], target: null },
    { end: () => ({ result: null }) },
  );
  const heard: OpEvent[] = [];
  await api.opSubscribe(plan.opId, new Channel<OpEvent>((event) => heard.push(event)));
  await api.opCancel(plan.opId);
  expect(heard).toEqual([]);
  const gone = await api.opExecute(plan.opId, new Channel<OpEvent>()).catch((e: unknown) => e);
  expect(codeOf(gone)).toBe(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND);

  const read = mockOps().startRead('Region latency', null, { end: () => ({ result: [] }) });
  await api.opDiscard(read);
  expect((await api.opList()).map((s) => s.opId)).toContain(read);
  await settle(5);
  await api.opDiscard(read);
  expect((await api.opList()).map((s) => s.opId)).not.toContain(read);
  const unknown = await api.opSubscribe(read, new Channel<OpEvent>()).catch((e: unknown) => e);
  expect(codeOf(unknown)).toBe(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND);
});

test('op_list holds operations, not plans, the latest started first, each in its state', async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
  const plan = mockOps().registerPlan(
    { class: 'bounded', title: 'x', changes: [], target: null },
    { end: () => ({ result: null }) },
  );
  const first = mockOps().startRead('Who am I', null, { end: () => ({ result: null }) });
  const second = mockOps().startRead('Doctor · lab', 'lab', { end: () => ({ error: taken }) });
  await settle(5);
  const list = await api.opList();
  expect(list.map((s) => [s.opId, s.state])).toEqual([
    [second, 'failed'],
    [first, 'finished'],
  ]);
  expect(list.some((s) => s.opId === plan.opId)).toBe(false);
});

test('a plan past its time is refused as expired, and spent', async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
  const heard: OpEvent[] = [];
  const view = mockOps().registerPlan(
    { class: 'bounded', title: 'x', changes: [], target: null },
    { end: () => ({ result: null }) },
  );
  expect(view.expiresAtMs - Date.now()).toBeGreaterThan(MOCK_PLAN_TTL_MS - 1000);
  await api.opSubscribe(view.opId, new Channel<OpEvent>((event) => heard.push(event)));
  setSystemTime(new Date(view.expiresAtMs + 1));
  const expired = await api.opExecute(view.opId, new Channel<OpEvent>()).catch((e: unknown) => e);
  expect((expired as api.IpcError).error).toMatchObject({
    code: DESKTOP_ERROR_CODES.PLAN_EXPIRED,
    fields: { opId: view.opId },
  });
  expect(heard.map((e) => e.kind)).toEqual(['failed']);
  const spent = await api.opExecute(view.opId, new Channel<OpEvent>()).catch((e: unknown) => e);
  expect(codeOf(spent)).toBe(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND);
});

test("the mock's plan lifetime is Rust's PLAN_TTL_MS", async () => {
  const managerRs = await Bun.file(
    new URL('../../../src-tauri/src/ops/manager.rs', import.meta.url),
  ).text();
  const expression = managerRs.match(/pub const PLAN_TTL_MS: u64 = ([\d_ *]+);/)?.[1];
  expect(expression, 'PLAN_TTL_MS in src-tauri/src/ops/manager.rs').toBeDefined();
  const value = (expression ?? '')
    .split('*')
    .map((factor) => Number(factor.replaceAll('_', '').trim()))
    .reduce((product, factor) => product * factor, 1);
  expect(MOCK_PLAN_TTL_MS).toBe(value);
});
