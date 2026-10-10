// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What the UI offers for an error (spec §5.1): a code it knows maps to an action, everything
// else is shown as it is. `apprafter::desktop::locked` maps to none: the lock gate shows it.
import { CORE_ERROR_CODES } from './generated/core-errors';
import type { UiError } from './generated/UiError';

export { CORE_ERROR_CODES };

export type ErrorAction =
  | { kind: 'add-target' }
  | { kind: 'renew-token' }
  | { kind: 'toolchain' }
  | { kind: 'machine-picker' }
  | { kind: 'import' }
  | { kind: 'backup-status' }
  | { kind: 'running-op' }
  | { kind: 'none' };

// The core codes the desktop acts on, from the generated list (the core passes them through
// `UiError.code`). `running-op` waits for the core code D.11/D.12 raise for a busy target.
const BY_CODE = new Map<string, ErrorAction['kind']>([
  [CORE_ERROR_CODES.TARGET_NOT_FOUND, 'add-target'],
  [CORE_ERROR_CODES.TARGET_TOKEN_REJECTED, 'renew-token'],
  [CORE_ERROR_CODES.PROVIDER_SERVER_TYPE_UNAVAILABLE, 'machine-picker'],
  [CORE_ERROR_CODES.ENV_TOOL_NOT_FOUND, 'toolchain'],
  [CORE_ERROR_CODES.ENV_CUE_NOT_FOUND, 'toolchain'],
  [CORE_ERROR_CODES.STATE_CORRUPT, 'import'],
  [CORE_ERROR_CODES.BACKUP_JOB_ACTIVE, 'backup-status'],
]);

export function errorAction(e: UiError): ErrorAction {
  if (e.code === CORE_ERROR_CODES.PROVIDER_HETZNER_API_ERROR) {
    // Only an authentication failure means the token; a 403 or a 5xx is something else.
    return { kind: e.fields.status === 401 ? 'renew-token' : 'none' };
  }
  return { kind: (e.code !== null && BY_CODE.get(e.code)) || 'none' };
}
