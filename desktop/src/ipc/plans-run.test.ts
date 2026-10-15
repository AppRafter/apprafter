// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What Task 10 adds to D.3d's plans.ts, on the IPC harness (D.3d's plans.test.ts runs on the mock
// engine): runRead's onStarted, endOf's refused follow, runPlan, isCancelled, failureOf and
// reportUnlessLocked.
import { afterEach, beforeEach, expect, spyOn, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { waitFor } from '@testing-library/react';
import { lockState } from '../test/fixtures';
import {
  cancelled,
  completed,
  failed,
  type Harness,
  installHarness,
  stage,
  uiError,
} from '../test/ipc';
import { settleIpc } from '../test/settle';
import { IpcError } from './api';
import { endedAwaySnapshot, resetEndedAway } from './away';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import { clearLive, operationsSnapshot, reattachAll, resetOperations } from './operations';
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
afterEach(async () => {
  await settleIpc();
  resetOperations();
  resetEndedAway();
  clearMocks();
});

test('runRead hands over the op id as soon as Rust answers, then resolves; the op is forgotten', async () => {
  const id = h.newOperation([stage(1, 3, 'Target'), completed({ target: 'x', groups: [] })]);
  const seen: number[] = [];
  const report = await runRead<{ target: string; groups: unknown[] }>(
    async () => id,
    (opId) => seen.push(opId),
  );
  expect(report).toEqual({ target: 'x', groups: [] });
  expect(seen).toEqual([id]);
  expect(h.of('op_discard').map((c) => c.args)).toEqual([{ opId: id }]);
  expect(operationsSnapshot().has(id)).toBe(false);
});

test('a failed read is OperationFailed carrying the UiError, and not a cancellation', async () => {
  const id = h.newOperation([
    failed(uiError('apprafter::target::token_rejected', 'rejected (HTTP 401)')),
  ]);
  const error = await runRead(async () => id).catch((e: unknown) => e);
  expect(error).toBeInstanceOf(OperationFailed);
  expect(failureOf(error).code).toBe('apprafter::target::token_rejected');
  expect(isCancelled(error)).toBe(false);
});

test('a cancelled read is isCancelled', async () => {
  const id = h.newOperation([cancelled()]);
  expect(isCancelled(await runRead(async () => id).catch((e: unknown) => e))).toBe(true);
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
  const id = h.newOperation([completed({ name: 'lab' })]);
  expect(await runPlan<{ name: string }>(id)).toEqual({ name: 'lab' });
  expect(h.of('op_execute')).toHaveLength(1);
  expect(h.of('op_discard').map((c) => c.args)).toEqual([{ opId: id }]);
});

test('a follow Rust refuses ends the read with that refusal instead of waiting forever', async () => {
  h.answer('op_subscribe', () => Promise.reject(uiError('apprafter::desktop::plan_not_found')));
  const id = h.newOperation([]);
  const error = await runRead(async () => id).catch((e: unknown) => e);
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

test('a plan whose screen went keeps its end for the app: not discarded, kept with its words', async () => {
  const id = h.newOperation([
    failed(uiError('apprafter::provider::sku_unavailable', 'cx22 is sold out')),
  ]);
  const error = await runPlan(id, undefined, {
    title: 'Add target lab',
    shown: () => false,
  }).catch((e: unknown) => e);
  expect(failureOf(error).message).toBe('cx22 is sold out');
  expect(h.of('op_discard')).toHaveLength(0);
  expect(endedAwaySnapshot()).toEqual([
    { opId: id, text: 'Add target lab failed: cx22 is sold out', failed: true },
  ]);
});

test('a plan whose screen went keeps a refused follow for the app too (review #14)', async () => {
  const id = h.newOperation([]); // runs on
  const ended = runPlan(id, undefined, { title: 'Add target lab', shown: () => false }).catch(
    (e: unknown) => e,
  );
  await waitFor(() => expect(h.of('op_execute')).toHaveLength(1));
  // A lock and an unlock: Rust dropped the subscription, the store follows again, and Rust
  // refuses that follow for a reason other than the lock.
  h.answer('op_subscribe', () =>
    Promise.reject(uiError('apprafter::desktop::plan_not_found', 'gone')),
  );
  clearLive();
  reattachAll();
  expect(failureOf(await ended).message).toBe('gone');
  expect(h.of('op_discard')).toHaveLength(0);
  expect(endedAwaySnapshot()).toEqual([
    { opId: id, text: 'Add target lab failed: gone', failed: true },
  ]);
});

test('a plan whose screen is still there is discarded and kept nowhere', async () => {
  const id = h.newOperation([completed({ name: 'lab' })]);
  await runPlan(id, undefined, { title: 'Add target lab', shown: () => true });
  expect(h.of('op_discard').map((c) => c.args)).toEqual([{ opId: id }]);
  expect(endedAwaySnapshot()).toEqual([]);
});

test('an end kept for the app says what happened: done, or cancelled', async () => {
  const done = h.newOperation([completed({ name: 'lab' })]);
  const stopped = h.newOperation([cancelled()]);
  const gone = { title: 'Rename prod', shown: () => false };
  await runPlan(done, undefined, gone);
  await runPlan(stopped, undefined, gone).catch(() => {});
  expect(endedAwaySnapshot()).toEqual([
    { opId: done, text: 'Rename prod: done.', failed: false },
    { opId: stopped, text: 'Rename prod was cancelled.', failed: true },
  ]);
});

// D.4a F18 item 4 (critic M-4; the owner's O15): a confirmed plan that the lock stopped before it
// began — an OS prompt still open, a typed secret the lock found wrong, or a confirm still on its
// way — is not silent after the unlock: the app says it was not started.
const STOP = 'Stop protecting the secrets of the target store at /s';
const notStarted = {
  opId: null,
  text: `${STOP} was not started: AppRafter locked before it began.`,
  failed: true,
};
const refusedBy = (code: string) => () => Promise.reject(uiError(code, code));

test('F18: a confirm the lock refused says after the unlock that it was not started', async () => {
  for (const code of [
    DESKTOP_ERROR_CODES.LOCKED,
    DESKTOP_ERROR_CODES.AUTH_CANCELLED,
    DESKTOP_ERROR_CODES.PLAN_NOT_FOUND,
  ]) {
    resetEndedAway();
    h.answer('op_execute', refusedBy(code));
    h.answer('lock_status', lockState({ locked: true, reason: 'manual' }));
    const id = h.newOperation([]);
    const error = await runPlan(id, 'lock password', { title: STOP, shown: () => false }).catch(
      (e: unknown) => e,
    );
    expect(failureOf(error).code).toBe(code);
    expect(endedAwaySnapshot()).toEqual([notStarted]);
  }
});

test('F18: its screen still shown as the refusal arrives (the lock event comes after it), it is kept all the same', async () => {
  h.answer('op_execute', refusedBy(DESKTOP_ERROR_CODES.AUTH_CANCELLED));
  h.answer('lock_status', lockState({ locked: true, reason: 'manual' }));
  const id = h.newOperation([]);
  await runPlan(id, undefined, { title: STOP, shown: () => true }).catch(() => {});
  expect(endedAwaySnapshot()).toEqual([notStarted]);
});

test('F18: a cancel the person clicked, or a plan gone with the app unlocked, is never kept', async () => {
  for (const code of [DESKTOP_ERROR_CODES.AUTH_CANCELLED, DESKTOP_ERROR_CODES.PLAN_NOT_FOUND]) {
    h.answer('op_execute', refusedBy(code));
    h.answer('lock_status', lockState({ locked: false }));
    const id = h.newOperation([]);
    await runPlan(id, undefined, { title: STOP, shown: () => false }).catch(() => {});
    await runPlan(id, undefined, { title: STOP, shown: () => true }).catch(() => {});
  }
  expect(endedAwaySnapshot()).toEqual([]);
});

test('F18: any other refusal is the screen’s to show, even with the app locked', async () => {
  h.answer('op_execute', refusedBy(DESKTOP_ERROR_CODES.AUTH_FAILED));
  h.answer('lock_status', lockState({ locked: true, reason: 'manual' }));
  const id = h.newOperation([]);
  await runPlan(id, 'not it', { title: STOP, shown: () => false }).catch(() => {});
  expect(endedAwaySnapshot()).toEqual([]);
});
