// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What the UI offers for an error (spec §5.1): a code it knows maps to an action, everything
// else is shown as it is. `apprafter::desktop::locked` maps to none: the lock gate shows it.
import type { UiError } from './generated/UiError';

export type ErrorAction =
  | { kind: 'add-target' }
  | { kind: 'renew-token' }
  | { kind: 'toolchain' }
  | { kind: 'machine-picker' }
  | { kind: 'import' }
  | { kind: 'backup-status' }
  | { kind: 'running-op' }
  | { kind: 'none' };

/** Core codes the desktop acts on; the core passes them through `UiError.code`. */
export const CORE_ERROR_CODES = {
  TARGET_NOT_FOUND: 'apprafter::target::not_found',
  TARGET_TOKEN_REJECTED: 'apprafter::target::token_rejected',
  TARGET_BUSY: 'apprafter::target::busy',
  HETZNER_API_ERROR: 'apprafter::provider::hetzner_api_error',
  SERVER_TYPE_UNAVAILABLE: 'apprafter::provider::server_type_unavailable',
  TOOL_NOT_FOUND: 'apprafter::env::tool_not_found',
  CUE_NOT_FOUND: 'apprafter::env::cue_not_found',
  STATE_CORRUPT: 'apprafter::state::corrupt',
  BACKUP_JOB_ACTIVE: 'apprafter::backup::job_active',
} as const;

const BY_CODE = new Map<string, ErrorAction['kind']>([
  [CORE_ERROR_CODES.TARGET_NOT_FOUND, 'add-target'],
  [CORE_ERROR_CODES.TARGET_TOKEN_REJECTED, 'renew-token'],
  [CORE_ERROR_CODES.TARGET_BUSY, 'running-op'],
  [CORE_ERROR_CODES.SERVER_TYPE_UNAVAILABLE, 'machine-picker'],
  [CORE_ERROR_CODES.TOOL_NOT_FOUND, 'toolchain'],
  [CORE_ERROR_CODES.CUE_NOT_FOUND, 'toolchain'],
  [CORE_ERROR_CODES.STATE_CORRUPT, 'import'],
  [CORE_ERROR_CODES.BACKUP_JOB_ACTIVE, 'backup-status'],
]);

export function errorAction(e: UiError): ErrorAction {
  if (e.code === CORE_ERROR_CODES.HETZNER_API_ERROR) {
    // Only an authentication failure means the token; a 403 or a 5xx is something else.
    return { kind: e.fields.status === 401 ? 'renew-token' : 'none' };
  }
  return { kind: (e.code !== null && BY_CODE.get(e.code)) || 'none' };
}
