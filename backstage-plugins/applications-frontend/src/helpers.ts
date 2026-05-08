// SPDX-License-Identifier: MIT
//
// Pure helpers used by the React components. Lives separately so
// it can be unit-tested via `bun test` (no DOM dependency).

import type { Application } from './types.ts';

/**
 * Distinct environment names declared across an Application list,
 * sorted alphabetically. Each name is taken from
 * `app.spec.environments`'s keys.
 */
export function environmentsOf(apps: Application[]): string[] {
  const names = new Set<string>();
  for (const app of apps) {
    if (app.spec.environments) {
      for (const k of Object.keys(app.spec.environments)) {
        names.add(k);
      }
    }
  }
  return Array.from(names).sort();
}

/**
 * Filter an Application list to only those that declare the named
 * environment override. Used by the per-environment tab strip so
 * each tab shows only the apps that actually opt into that env.
 */
export function applicationsForEnvironment(
  apps: Application[],
  environment: string,
): Application[] {
  return apps.filter(
    (app) => app.spec.environments?.[environment] !== undefined,
  );
}
