// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { discardPlansOf, holdPlan, releasePlan, resetHeldPlans } from './heldPlans';

let discarded: number[];

beforeEach(() => {
  discarded = [];
  resetHeldPlans();
  mockIPC((cmd, args) => {
    if (cmd === 'op_discard') discarded.push((args as { opId: number }).opId);
    return null;
  });
});
afterEach(() => {
  resetHeldPlans();
  clearMocks();
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test('a tab that closes discards the plans its confirms hold, and only its own', async () => {
  holdPlan('tab-a', 1);
  holdPlan('tab-a', 2);
  holdPlan('tab-b', 3);
  discardPlansOf('tab-a');
  await settle();
  expect(discarded.sort()).toEqual([1, 2]);
  discardPlansOf('tab-b');
  await settle();
  expect(discarded.sort()).toEqual([1, 2, 3]);
});

test('a plan that started, or that its confirm discarded, is not discarded again', async () => {
  holdPlan('tab-a', 1);
  releasePlan(1);
  discardPlansOf('tab-a');
  await settle();
  expect(discarded).toEqual([]);
});

test('a plan made for a tab that closed meanwhile is discarded at once', async () => {
  discardPlansOf('tab-a');
  holdPlan('tab-a', 7);
  await settle();
  expect(discarded).toEqual([7]);
});
