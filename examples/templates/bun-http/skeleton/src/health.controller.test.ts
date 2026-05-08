// SPDX-License-Identifier: MIT
//
// Smoke test — verifies the controller class loads (decorators
// resolve, OneBun runtime is present). Operators replace this
// with real tests for their own controllers.

import { test, expect } from 'bun:test';
import { HealthController } from './health.controller.ts';

test('HealthController loads with OneBun decorators applied', () => {
  expect(HealthController).toBeDefined();
  // The class must have an instantiable shape (no thrown errors at decoration time).
  expect(typeof HealthController).toBe('function');
  expect(HealthController.name).toBe('HealthController');
});

test('HealthController exposes async health and ready methods', () => {
  const proto = HealthController.prototype as unknown as Record<string, unknown>;
  expect(typeof proto.health).toBe('function');
  expect(typeof proto.ready).toBe('function');
});
