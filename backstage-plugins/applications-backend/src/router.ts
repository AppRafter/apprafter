// SPDX-License-Identifier: MIT
//
// Pure async handlers for the Backstage backend plugin. v0.1.33
// returns stub responses; v0.1.34 (sub-phase 1.10b) swaps the
// `ApplicationStore` impl for a real kube client. The `Express` /
// Backstage glue (router.use, plugin registration) lands in
// v0.1.34 once the store is real.

import type { Application } from './types.ts';

/**
 * Storage abstraction the handlers depend on. In v0.1.33 the only
 * implementation is `StubApplicationStore` (returns empty / null).
 * v0.1.34 introduces `KubeApplicationStore` that proxies the kube
 * apiserver via the in-cluster service-account token.
 */
export interface ApplicationStore {
  list(namespace?: string): Promise<Application[]>;
  get(namespace: string, name: string): Promise<Application | null>;
}

/** No-op store for v0.1.33. Always returns empty / null. */
export class StubApplicationStore implements ApplicationStore {
  async list(_namespace?: string): Promise<Application[]> {
    return [];
  }
  async get(_namespace: string, _name: string): Promise<Application | null> {
    return null;
  }
}

export interface ListApplicationsResponse {
  items: Application[];
}

export interface GetApplicationResponse {
  application: Application | null;
  notFound: boolean;
}

/**
 * Handler for `GET /api/applications` (and
 * `GET /api/applications/:namespace`).
 */
export async function listApplicationsHandler(
  store: ApplicationStore,
  namespace?: string,
): Promise<ListApplicationsResponse> {
  const items = await store.list(namespace);
  return { items };
}

/**
 * Handler for `GET /api/applications/:namespace/:name`.
 * Returns `{ application: null, notFound: true }` when the named
 * Application doesn't exist; the Backstage router layer (v0.1.34)
 * translates that into a 404.
 */
export async function getApplicationHandler(
  store: ApplicationStore,
  namespace: string,
  name: string,
): Promise<GetApplicationResponse> {
  const application = await store.get(namespace, name);
  return { application, notFound: application === null };
}
