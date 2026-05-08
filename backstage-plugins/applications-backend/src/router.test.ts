// SPDX-License-Identifier: MIT
import { test, expect } from 'bun:test';
import {
  StubApplicationStore,
  listApplicationsHandler,
  getApplicationHandler,
  type ApplicationStore,
} from './router.ts';
import type { Application } from './types.ts';

test('listApplicationsHandler returns empty items from the stub store', async () => {
  const store = new StubApplicationStore();
  const result = await listApplicationsHandler(store);
  expect(result.items).toEqual([]);
});

test('listApplicationsHandler passes namespace through to the store', async () => {
  let received: string | undefined;
  const store: ApplicationStore = {
    async list(namespace) {
      received = namespace;
      return [];
    },
    async get() {
      return null;
    },
  };
  await listApplicationsHandler(store, 'default');
  expect(received).toBe('default');
});

test('getApplicationHandler returns notFound=true when store returns null', async () => {
  const store = new StubApplicationStore();
  const result = await getApplicationHandler(store, 'default', 'nonexistent');
  expect(result.application).toBeNull();
  expect(result.notFound).toBe(true);
});

test('getApplicationHandler returns notFound=false when store returns a record', async () => {
  const sample: Application = {
    apiVersion: 'apprafter.io/v1alpha1',
    kind: 'Application',
    metadata: { name: 'web', namespace: 'default' },
    spec: { base: { image: 'x' } },
  };
  const store: ApplicationStore = {
    async list() {
      return [sample];
    },
    async get(_ns, _name) {
      return sample;
    },
  };
  const result = await getApplicationHandler(store, 'default', 'web');
  expect(result.notFound).toBe(false);
  expect(result.application).toEqual(sample);
});

test('listApplicationsHandler forwards the store result', async () => {
  const sample: Application = {
    apiVersion: 'apprafter.io/v1alpha1',
    kind: 'Application',
    metadata: { name: 'web', namespace: 'default' },
    spec: {},
  };
  const store: ApplicationStore = {
    async list() {
      return [sample, sample];
    },
    async get() {
      return null;
    },
  };
  const result = await listApplicationsHandler(store);
  expect(result.items.length).toBe(2);
});
