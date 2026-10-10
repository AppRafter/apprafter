// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, expect, mock, test } from 'bun:test';
import { newScope, resetLifecycle, sessionLocked, sessionScope } from './lifecycle';

afterEach(() => resetLifecycle());

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
  const parent = newScope(null);
  const child = newScope(parent.scope);
  const onChild = mock();
  child.scope.onGone(onChild);
  child.end();
  parent.end();
  expect(onChild).toHaveBeenCalledTimes(1);
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
