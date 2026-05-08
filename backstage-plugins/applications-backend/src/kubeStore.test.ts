// SPDX-License-Identifier: MIT
import { test, expect } from 'bun:test';
import { KubeApplicationStore, inClusterConfig } from './kubeStore.ts';
import type { Application } from './types.ts';

const SAMPLE: Application = {
  apiVersion: 'apprafter.io/v1alpha1',
  kind: 'Application',
  metadata: { name: 'web', namespace: 'default' },
  spec: { base: { image: 'ghcr.io/acme/web:1.0' } },
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

function recordingFetch(): {
  fn: typeof fetch;
  calls: Array<{ url: string; init: RequestInit | undefined }>;
  setResponse: (r: Response) => void;
} {
  const calls: Array<{ url: string; init: RequestInit | undefined }> = [];
  let nextResponse: Response = jsonResponse({ items: [] });
  // Bun's `typeof fetch` carries a `preconnect` static method we
  // never invoke here — cast through `unknown` so the mock fn's
  // shape is accepted as a fetch impl.
  const fn = (async (input: string | URL | Request, init?: RequestInit) => {
    calls.push({ url: String(input), init });
    return nextResponse;
  }) as unknown as typeof fetch;
  return {
    fn,
    calls,
    setResponse: (r) => {
      nextResponse = r;
    },
  };
}

test('list() with no namespace hits the cluster-wide URL', async () => {
  const f = recordingFetch();
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 't',
    fetchImpl: f.fn,
  });
  await store.list();
  expect(f.calls.length).toBe(1);
  expect(f.calls[0].url).toBe('https://kube/apis/apprafter.io/v1alpha1/applications');
});

test('list("default") hits the namespaced URL', async () => {
  const f = recordingFetch();
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 't',
    fetchImpl: f.fn,
  });
  await store.list('default');
  expect(f.calls[0].url).toBe(
    'https://kube/apis/apprafter.io/v1alpha1/namespaces/default/applications',
  );
});

test('list() sets Authorization: Bearer <token>', async () => {
  const f = recordingFetch();
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 'sa-token',
    fetchImpl: f.fn,
  });
  await store.list();
  const headers = (f.calls[0].init?.headers ?? {}) as Record<string, string>;
  expect(headers.Authorization).toBe('Bearer sa-token');
  expect(headers.Accept).toBe('application/json');
});

test('list() filters non-Application items via isApplication', async () => {
  const f = recordingFetch();
  f.setResponse(
    jsonResponse({
      items: [
        SAMPLE,
        { apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'noise' } },
        { random: 'object' },
      ],
    }),
  );
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 't',
    fetchImpl: f.fn,
  });
  const items = await store.list();
  expect(items).toEqual([SAMPLE]);
});

test('list() throws on non-2xx response with status + body in message', async () => {
  const f = recordingFetch();
  f.setResponse(new Response('forbidden', { status: 403 }));
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 't',
    fetchImpl: f.fn,
  });
  await expect(store.list()).rejects.toThrow(/403.*forbidden/);
});

test('get() returns null on 404', async () => {
  const f = recordingFetch();
  f.setResponse(new Response('{}', { status: 404 }));
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 't',
    fetchImpl: f.fn,
  });
  const result = await store.get('default', 'missing');
  expect(result).toBeNull();
});

test('get() returns the Application when valid', async () => {
  const f = recordingFetch();
  f.setResponse(jsonResponse(SAMPLE));
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 't',
    fetchImpl: f.fn,
  });
  const result = await store.get('default', 'web');
  expect(result).toEqual(SAMPLE);
});

test('get() returns null when response shape is not an Application', async () => {
  const f = recordingFetch();
  f.setResponse(
    jsonResponse({ apiVersion: 'v1', kind: 'ConfigMap', metadata: { name: 'x' } }),
  );
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 't',
    fetchImpl: f.fn,
  });
  const result = await store.get('default', 'web');
  expect(result).toBeNull();
});

test('get() throws on non-2xx non-404 response', async () => {
  const f = recordingFetch();
  f.setResponse(new Response('boom', { status: 500 }));
  const store = new KubeApplicationStore({
    apiServer: 'https://kube',
    token: 't',
    fetchImpl: f.fn,
  });
  await expect(store.get('default', 'web')).rejects.toThrow(/500.*boom/);
});

test('inClusterConfig() throws when KUBERNETES_SERVICE_HOST is unset', async () => {
  const prior = process.env.KUBERNETES_SERVICE_HOST;
  delete process.env.KUBERNETES_SERVICE_HOST;
  try {
    await expect(inClusterConfig()).rejects.toThrow(
      /KUBERNETES_SERVICE_HOST is not set/,
    );
  } finally {
    if (prior !== undefined) process.env.KUBERNETES_SERVICE_HOST = prior;
  }
});
