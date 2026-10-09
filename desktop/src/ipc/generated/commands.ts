// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Generated from desktop/ipc/src/commands.rs by `just desktop-ipc-types`. Do not edit.

/** Every command the shell registers. */
export const COMMANDS = [
  'activity',
  'app_info',
  'lock_now',
  'lock_status',
  'op_cancel',
  'op_discard',
  'op_execute',
  'op_list',
  'op_subscribe',
  'op_unsubscribe',
  'quit',
  'settings_get',
  'settings_set',
  'unlock',
  'unlock_with_password',
  'window_ready',
] as const;

/** What a locked app still answers; everything else fails with `DESKTOP_ERROR_CODES.LOCKED`. */
export const ALLOWED_WHILE_LOCKED = [
  'app_info',
  'lock_status',
  'quit',
  'settings_get',
  'unlock',
  'unlock_with_password',
  'window_ready',
] as const;
