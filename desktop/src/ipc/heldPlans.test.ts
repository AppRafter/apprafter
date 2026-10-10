// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { settleIpc } from '../test/settle';
import { holdPlan, releasePlan, resetHeldPlans } from './heldPlans';
import { newScope, resetLifecycle, sessionLocked, sessionScope } from './lifecycle';

let discarded: number[];

beforeEach(() => {
  discarded = [];
  resetHeldPlans();
  resetLifecycle();
  mockIPC((cmd, args) => {
    if (cmd === 'op_discard') discarded.push((args as { opId: number }).opId);
    return null;
  });
});
afterEach(async () => {
  await settleIpc();
  resetHeldPlans();
  resetLifecycle();
  clearMocks();
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test('a screen that goes discards the plans its confirms hold, and only its own, once', async () => {
  const a = newScope(sessionScope());
  const b = newScope(sessionScope());
  holdPlan(a.scope, 1);
  holdPlan(a.scope, 2);
  holdPlan(b.scope, 3);
  a.end();
  a.end();
  await settle();
  expect(discarded.sort()).toEqual([1, 2]);
  b.end();
  await settle();
  expect(discarded.sort()).toEqual([1, 2, 3]);
});

test('a plan that started, or that its confirm discarded, is not discarded again', async () => {
  const a = newScope(sessionScope());
  holdPlan(a.scope, 1);
  releasePlan(1);
  a.end();
  await settle();
  expect(discarded).toEqual([]);
});

test('a plan made for a screen that went meanwhile is discarded at once', async () => {
  const a = newScope(sessionScope());
  a.end();
  holdPlan(a.scope, 7);
  await settle();
  expect(discarded).toEqual([7]);
  releasePlan(7);
  await settle();
  expect(discarded).toEqual([7]);
});

test('an overlay goes with its view, and every screen with the lock', async () => {
  const view = newScope(sessionScope());
  const overlay = newScope(view.scope);
  const other = newScope(sessionScope());
  holdPlan(overlay.scope, 4);
  holdPlan(other.scope, 5);
  view.end();
  await settle();
  expect(discarded).toEqual([4]);
  sessionLocked();
  await settle();
  expect(discarded).toEqual([4, 5]);
});
