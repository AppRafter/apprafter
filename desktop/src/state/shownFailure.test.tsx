// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// WI-483: a change's failure a screen shows is kept for after the unlock when a lock comes while
// it shows — from the moment the screen is told, never a render later, and once. Never a refusal,
// a failure cleared or replaced, one closed with its screen, one a hidden tab holds, or one told
// to a screen that is gone. The lock is the session's end (ipc/lifecycle.ts), as LockGate ends it.
import { afterEach, describe, expect, test } from 'bun:test';
import { act, cleanup, render, screen } from '@testing-library/react';
import { Activity } from 'react';
import { endedAwaySnapshot, resetEndedAway } from '../ipc/away';
import type { UiError } from '../ipc/generated/UiError';
import { newScope, resetLifecycle, sessionLocked, sessionScope } from '../ipc/lifecycle';
import { ScopeContext } from './scope';
import { type ShowFailure, useShownFailure } from './shownFailure';

afterEach(() => {
  cleanup();
  resetEndedAway();
  resetLifecycle();
});

const SOLD_OUT_CODE = 'apprafter::provider::sku_unavailable';
const SOLD_OUT: UiError = {
  code: SOLD_OUT_CODE,
  message: 'cx22 is sold out in nbg1',
  help: null,
  causes: [],
  fields: {},
};
const TAKEN: UiError = { ...SOLD_OUT, code: 'apprafter::target::exists', message: 'lab is taken' };
const KEPT = {
  opId: null,
  text: 'Add target lab failed: cx22 is sold out in nbg1',
  failed: true,
  code: SOLD_OUT_CODE,
};

/** A screen in its own scope under the session, in an <Activity> its tab switches. */
function setup(strict = false) {
  const own = newScope(sessionScope());
  const told: { show: ShowFailure } = { show: () => {} };
  function Panel() {
    const [failure, show] = useShownFailure();
    told.show = show;
    return failure === null ? null : <p>{failure.error.message}</p>;
  }
  const Host = ({ shown }: { shown: boolean }) => (
    <ScopeContext value={own.scope}>
      <Activity mode={shown ? 'visible' : 'hidden'}>
        <Panel />
      </Activity>
    </ScopeContext>
  );
  const view = render(<Host shown />, { reactStrictMode: strict });
  return {
    own,
    told,
    show: (error: UiError | null, change?: string) => act(() => told.show(error, change)),
    tab: (shown: boolean) => view.rerender(<Host shown={shown} />),
    unmount: view.unmount,
  };
}

const lock = () =>
  act(() => {
    sessionLocked();
  });

describe.each([false, true])('StrictMode %p', (strict) => {
  test('a change’s failure shown when the app locks is kept once, in its line, with its code', () => {
    const s = setup(strict);
    s.show(SOLD_OUT, 'Add target lab');
    expect(screen.getByText(SOLD_OUT.message)).toBeDefined();
    expect(endedAwaySnapshot()).toEqual([]);
    lock();
    expect(endedAwaySnapshot()).toEqual([KEPT]);
    lock();
    expect(endedAwaySnapshot()).toEqual([KEPT]);
  });

  test('shown again after its tab hid it, it is kept again', () => {
    const s = setup(strict);
    s.show(SOLD_OUT, 'Add target lab');
    s.tab(false);
    s.tab(true);
    lock();
    expect(endedAwaySnapshot()).toEqual([KEPT]);
  });
});

test('a refusal is never kept: nothing ran', () => {
  const s = setup();
  s.show(SOLD_OUT);
  expect(screen.getByText(SOLD_OUT.message)).toBeDefined();
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('a failure cleared before the lock is not kept', () => {
  const s = setup();
  s.show(SOLD_OUT, 'Add target lab');
  s.show(null);
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('a failure replaced before the lock: only the one shown is kept', () => {
  const s = setup();
  s.show(TAKEN, 'Rename prod');
  s.show(SOLD_OUT, 'Add target lab');
  lock();
  expect(endedAwaySnapshot()).toEqual([KEPT]);
});

test('a failure replaced by a refusal: nothing is kept', () => {
  const s = setup();
  s.show(SOLD_OUT, 'Add target lab');
  s.show(TAKEN);
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('its screen closed first (its dialog, its tab): nothing is kept, then or at the lock', () => {
  const s = setup();
  s.show(SOLD_OUT, 'Add target lab');
  act(() => s.own.end());
  expect(endedAwaySnapshot()).toEqual([]);
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('hidden by its tab when the lock comes, it is not kept: it is not shown', () => {
  const s = setup();
  s.show(SOLD_OUT, 'Add target lab');
  s.tab(false);
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

// The race forced, never sampled: the screen is told, and the lock ends the session, before
// React commits the panel. Kept from the moment it was told.
test('told in the same commit as the lock, before its panel shows, it is kept', () => {
  const s = setup();
  act(() => {
    s.told.show(SOLD_OUT, 'Add target lab');
    expect(screen.queryByText(SOLD_OUT.message)).toBeNull();
    sessionLocked();
  });
  expect(endedAwaySnapshot()).toEqual([KEPT]);
});

test('told after its screen went (the app kept the run’s end itself), nothing more is kept', () => {
  const s = setup();
  lock();
  s.show(SOLD_OUT, 'Add target lab');
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
  const t = setup();
  act(() => t.own.end());
  t.show(SOLD_OUT, 'Add target lab');
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('unmounted without a lock (the quit), it keeps nothing', () => {
  const s = setup();
  s.show(SOLD_OUT, 'Add target lab');
  s.unmount();
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('a cancel is kept in words, with no code', () => {
  const s = setup();
  s.show({ ...SOLD_OUT, code: 'apprafter::op::cancelled', message: 'cancelled' }, 'Rename prod');
  lock();
  expect(endedAwaySnapshot()).toEqual([
    { opId: null, text: 'Rename prod was cancelled.', failed: true },
  ]);
});
