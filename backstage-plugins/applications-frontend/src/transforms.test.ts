// SPDX-License-Identifier: MIT
import { test, expect } from 'bun:test';
import { applicationsToRows, applicationToRow } from './transforms.ts';
import type { Application } from './types.ts';

const FULL: Application = {
  apiVersion: 'apprafter.io/v1alpha1',
  kind: 'Application',
  metadata: { name: 'web', namespace: 'default' },
  spec: {
    base: {
      image: 'ghcr.io/acme/web:1.0',
      replicas: 3,
      expose: { port: 8080, public: false, network: 'internal' },
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

test('applicationToRow projects every observable field', () => {
  const row = applicationToRow(FULL);
  expect(row).toEqual({
    name: 'web',
    namespace: 'default',
    image: 'ghcr.io/acme/web:1.0',
    replicas: 3,
    phase: 'Ready',
    endpointURL: 'http://web.default.svc.cluster.local:80',
    ready: 'True',
  });
});

test('applicationsToRows preserves order', () => {
  const a = { ...FULL, metadata: { name: 'a' } };
  const b = { ...FULL, metadata: { name: 'b' } };
  const c = { ...FULL, metadata: { name: 'c' } };
  const rows = applicationsToRows([a, b, c]);
  expect(rows.map((r) => r.name)).toEqual(['a', 'b', 'c']);
});

test('applicationToRow falls back to empty / 0 when fields are missing', () => {
  const minimal: Application = {
    apiVersion: 'apprafter.io/v1alpha1',
    kind: 'Application',
    metadata: { name: 'x' },
    spec: {},
  };
  expect(applicationToRow(minimal)).toEqual({
    name: 'x',
    namespace: '',
    image: '',
    replicas: 0,
    phase: '',
    endpointURL: '',
    ready: '',
  });
});

test('applicationToRow ready=False when Ready condition is False', () => {
  const failing: Application = {
    ...FULL,
    status: {
      ...FULL.status!,
      phase: 'Failed',
      conditions: [
        {
          type: 'Ready',
          status: 'False',
          lastTransitionTime: '2026-05-08T12:01:00Z',
          reason: 'ApplyFailed',
          message: 'Apply step returned an error',
        },
      ],
    },
  };
  expect(applicationToRow(failing).ready).toBe('False');
  expect(applicationToRow(failing).phase).toBe('Failed');
});

test('applicationToRow ready="" when no Ready condition is present', () => {
  const noReady: Application = {
    ...FULL,
    status: {
      ...FULL.status!,
      conditions: [
        {
          type: 'Progressing',
          status: 'True',
          lastTransitionTime: '2026-05-08T12:01:00Z',
          reason: 'Working',
          message: 'In progress',
        },
      ],
    },
  };
  expect(applicationToRow(noReady).ready).toBe('');
});
