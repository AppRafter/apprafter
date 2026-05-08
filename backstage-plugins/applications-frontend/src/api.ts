// SPDX-License-Identifier: MIT
//
// Frontend → backend contract. v0.1.35 declares the interface +
// a string ID that v0.1.36 converts into a real
// `@backstage/core-plugin-api::ApiRef<ApplicationsApi>`.

import type { Application } from './types.ts';

/** What the frontend expects from a backend impl. */
export interface ApplicationsApi {
  /** List Applications, optionally scoped to one namespace. */
  listApplications(namespace?: string): Promise<Application[]>;
  /** Fetch one Application; resolves to `null` on 404. */
  getApplication(namespace: string, name: string): Promise<Application | null>;
}

/**
 * Stable ID for the API. v0.1.36 wraps this in the Backstage
 * `createApiRef<ApplicationsApi>({ id: applicationsApiRefId })`
 * pattern; v0.1.35 ships it as a plain string so consumers can
 * already register an impl in their dep-injection container of
 * choice.
 */
export const applicationsApiRefId = 'apprafter.applications' as const;
