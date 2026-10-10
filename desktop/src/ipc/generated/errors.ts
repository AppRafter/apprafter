// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Generated from desktop/ipc/src/errors.rs by `just desktop-ipc-types`. Do not edit.

/** The codes the desktop itself raises as `UiError.code`; core codes pass through. */
export const DESKTOP_ERROR_CODES = {
  AUTH_BUSY: 'apprafter::desktop::auth_busy',
  AUTH_CANCELLED: 'apprafter::desktop::auth_cancelled',
  AUTH_FAILED: 'apprafter::desktop::auth_failed',
  AUTH_UNAVAILABLE: 'apprafter::desktop::auth_unavailable',
  CLOSING: 'apprafter::desktop::closing',
  DRAFT_EXPIRED: 'apprafter::desktop::draft_expired',
  DRAFT_NOT_FOUND: 'apprafter::desktop::draft_not_found',
  INTERNAL: 'apprafter::desktop::internal',
  LOCKED: 'apprafter::desktop::locked',
  PLAN_EXPIRED: 'apprafter::desktop::plan_expired',
  PLAN_NOT_FOUND: 'apprafter::desktop::plan_not_found',
  RELATIVE_PATH: 'apprafter::desktop::relative_path',
  SETTINGS_IO: 'apprafter::desktop::settings_io',
} as const;
