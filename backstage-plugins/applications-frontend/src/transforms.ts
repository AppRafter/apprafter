// SPDX-License-Identifier: MIT
//
// Pure-data transforms used by the future React table. Lives
// here as a separate module so v0.1.35 can ship + test the data
// shape without pulling in React.

import type { Application, ApplicationCondition } from './types.ts';

/** One row in the Applications table. All fields are display-ready strings or numbers. */
export interface ApplicationRow {
  name: string;
  namespace: string;
  image: string;
  replicas: number;
  phase: string;
  endpointURL: string;
  /** "Ready" condition status: "True" | "False" | "Unknown" | "" (when no condition). */
  ready: string;
}

/** Project an Application list into table rows. */
export function applicationsToRows(apps: Application[]): ApplicationRow[] {
  return apps.map(applicationToRow);
}

export function applicationToRow(app: Application): ApplicationRow {
  const base = app.spec.base;
  return {
    name: app.metadata.name,
    namespace: app.metadata.namespace ?? '',
    image: base?.image ?? '',
    replicas: base?.replicas ?? 0,
    phase: app.status?.phase ?? '',
    endpointURL: app.status?.endpointURL ?? '',
    ready: readyStatus(app.status?.conditions),
  };
}

function readyStatus(conditions: ApplicationCondition[] | undefined): string {
  if (!conditions) return '';
  const ready = conditions.find((c) => c.type === 'Ready');
  return ready?.status ?? '';
}
