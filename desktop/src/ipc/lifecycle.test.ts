// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, mock, test } from 'bun:test';
import { newScope, type Scope, sessionLocked, sessionScope } from './lifecycle';

// No reset here: the preload swaps the session after every test of every file (test/dom.ts),
// which the last two tests show.

test('a scope ends once: what is registered runs once, an unregistered one never', () => {
  const { scope, end } = newScope(null);
  const a = mock();
  const b = mock();
  scope.onGone(a);
  const offB = scope.onGone(b);
  offB();
  expect(scope.gone()).toBe(false);
  end();
  end();
  expect(scope.gone()).toBe(true);
  expect(a).toHaveBeenCalledTimes(1);
  expect(b).not.toHaveBeenCalled();
});

test('what registers after the end runs at once', () => {
  const { scope, end } = newScope(null);
  end();
  const late = mock();
  scope.onGone(late);
  expect(late).toHaveBeenCalledTimes(1);
});

test('a child ends with its parent, and a child of a parent gone is gone at once', () => {
  const parent = newScope(null);
  const child = newScope(parent.scope);
  const onChild = mock();
  child.scope.onGone(onChild);
  parent.end();
  expect(child.scope.gone()).toBe(true);
  expect(onChild).toHaveBeenCalledTimes(1);
  expect(newScope(parent.scope).scope.gone()).toBe(true);
});

test('a child that ends on its own leaves its parent, and the parent lives on', () => {
  // Its registration with the parent is dropped: an overlay or a tab that closes leaves nothing
  // on the session (what it captured: its reads, its held plans) until the next lock.
  const off = mock();
  const parent: Scope = { gone: () => false, onGone: mock(() => off) };
  const child = newScope(parent);
  const onChild = mock();
  child.scope.onGone(onChild);
  expect(off).not.toHaveBeenCalled();
  child.end();
  expect(off).toHaveBeenCalledTimes(1);
  expect(onChild).toHaveBeenCalledTimes(1);
  // A real parent lives on, and its end does not end the child again.
  const real = newScope(null);
  const kid = newScope(real.scope);
  const onKid = mock();
  kid.scope.onGone(onKid);
  kid.end();
  expect(real.scope.gone()).toBe(false);
  real.end();
  expect(onKid).toHaveBeenCalledTimes(1);
});

test('the lock ends the session and its screens; the next session starts unended', () => {
  const before = sessionScope();
  const screen = newScope(before);
  sessionLocked();
  expect(before.gone()).toBe(true);
  expect(screen.scope.gone()).toBe(true);
  expect(sessionScope()).not.toBe(before);
  expect(sessionScope().gone()).toBe(false);
});

// bun runs every test file in one process, so the session is shared by all of them: a read, a
// draft or a plan a test leaves registered on it would be ended by the next lock any later test
// makes, into that test's mock. The preload swaps the session after every test (test/dom.ts).
let leftover = mock();

test('a test that leaves a screen on the session (1 of 2)', () => {
  leftover = mock();
  sessionScope().onGone(leftover);
  newScope(sessionScope()).scope.onGone(leftover);
  expect(sessionScope().gone()).toBe(false);
});

test("… does not hand it to the next test's lock (2 of 2)", () => {
  sessionLocked();
  expect(leftover).not.toHaveBeenCalled();
});
