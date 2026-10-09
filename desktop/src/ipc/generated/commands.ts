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
  'op_plan_target_add',
  'op_plan_target_machine',
  'op_plan_target_remove',
  'op_plan_target_rename',
  'op_plan_target_renew',
  'op_plan_target_use',
  'op_start_doctor',
  'op_start_machine_catalogue',
  'op_start_region_latencies',
  'op_start_verify_token',
  'op_start_whoami',
  'op_subscribe',
  'op_unsubscribe',
  'quit',
  'settings_get',
  'settings_set',
  'ssh_key_candidates',
  'ssh_key_inspect',
  'target_draft_discard',
  'target_list',
  'target_show',
  'theme_apply',
  'toolchain_status',
  'unlock',
  'unlock_with_password',
  'whoami',
  'window_ready',
] as const;

/** What a locked app still answers; everything else fails with `DESKTOP_ERROR_CODES.LOCKED`. */
export const ALLOWED_WHILE_LOCKED = [
  'app_info',
  'lock_status',
  'quit',
  'settings_get',
  'theme_apply',
  'unlock',
  'unlock_with_password',
  'window_ready',
] as const;
