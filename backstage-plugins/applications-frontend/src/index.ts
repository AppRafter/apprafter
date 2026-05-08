// SPDX-License-Identifier: MIT
//
// Public surface of @apprafter/applications-frontend.

export type {
  Application,
  ApplicationBaseSpec,
  ApplicationCondition,
  ApplicationExpose,
  ApplicationSpec,
  ApplicationStatus,
  ObjectMeta,
} from './types.ts';

export type { ApplicationsApi } from './api.ts';
export { applicationsApiRefId } from './api.ts';

export type { ApplicationRow } from './transforms.ts';
export { applicationToRow, applicationsToRows } from './transforms.ts';

export {
  applicationsForEnvironment,
  environmentsOf,
} from './helpers.ts';

export type { ApplicationsTableProps } from './components/ApplicationsTable.tsx';
export { ApplicationsTable } from './components/ApplicationsTable.tsx';

export type { ApplicationDetailProps } from './components/ApplicationDetail.tsx';
export { ApplicationDetail } from './components/ApplicationDetail.tsx';

export type { EnvironmentTabsProps } from './components/EnvironmentTabs.tsx';
export { EnvironmentTabs } from './components/EnvironmentTabs.tsx';
