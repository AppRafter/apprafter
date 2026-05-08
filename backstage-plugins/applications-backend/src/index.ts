// SPDX-License-Identifier: MIT
//
// Public surface of @apprafter/applications-backend.

export type {
  Application,
  ApplicationBaseSpec,
  ApplicationCondition,
  ApplicationExpose,
  ApplicationSpec,
  ApplicationStatus,
  ObjectMeta,
} from './types.ts';

export { isApplication } from './types.ts';

export type {
  ApplicationStore,
  ListApplicationsResponse,
  GetApplicationResponse,
} from './router.ts';

export {
  StubApplicationStore,
  listApplicationsHandler,
  getApplicationHandler,
} from './router.ts';
