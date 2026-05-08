// SPDX-License-Identifier: MIT
import { test, expect } from 'bun:test';
import { isApplication, type Application } from './types.ts';

test('isApplication accepts a well-formed manifest', () => {
  const app: Application = {
    apiVersion: 'apprafter.io/v1alpha1',
    kind: 'Application',
    metadata: { name: 'web', namespace: 'default' },
    spec: {
      base: {
        image: 'ghcr.io/acme/web:1.0',
        replicas: 3,
        expose: { port: 8080, public: false, network: 'internal' },
        env: { LOG_LEVEL: 'info' },
      },
      environments: {
        prod: { replicas: 5 },
      },
    },
    status: {
      phase: 'Ready',
      observedGeneration: 1,
      endpointURL: 'http://web.default.svc.cluster.local:80',
      conditions: [
        {
          type: 'Ready',
          status: 'True',
          lastTransitionTime: '2026-05-08T12:00:00Z',
          reason: 'ReconcileSucceeded',
          message: 'Reconcile completed.',
        },
      ],
    },
  };
  expect(isApplication(app)).toBe(true);
});

test('isApplication rejects wrong apiVersion', () => {
  const wrong = {
    apiVersion: 'v1',
    kind: 'Application',
    metadata: { name: 'web' },
    spec: {},
  };
  expect(isApplication(wrong)).toBe(false);
});

test('isApplication rejects wrong kind', () => {
  const wrong = {
    apiVersion: 'apprafter.io/v1alpha1',
    kind: 'ConfigMap',
    metadata: { name: 'web' },
    spec: {},
  };
  expect(isApplication(wrong)).toBe(false);
});

test('isApplication rejects null and primitives', () => {
  expect(isApplication(null)).toBe(false);
  expect(isApplication(undefined)).toBe(false);
  expect(isApplication('string')).toBe(false);
  expect(isApplication(42)).toBe(false);
});

test('isApplication round-trips through JSON', () => {
  const app: Application = {
    apiVersion: 'apprafter.io/v1alpha1',
    kind: 'Application',
    metadata: { name: 'web' },
    spec: {},
  };
  const parsed = JSON.parse(JSON.stringify(app));
  expect(isApplication(parsed)).toBe(true);
});
