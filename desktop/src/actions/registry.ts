// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ApiCommand } from '../ipc/api';

/**
 * Every GUI action that runs a CLI leaf command, and the command it runs. `desktop/coverage.toml`
 * maps each visible leaf of `docs/reference/cli/commands.json` to one of these ids or to a status
 * (design spec §2, §5.3); coverage.test.ts fails on a leaf with neither, on an id whose command
 * the shell does not register, and on a planned leaf in a closed slice. A slice adds its ids
 * with the surface that uses them.
 */
export const ACTION_IDS = [
  'target.add',
  'target.list',
  'target.show',
  'target.use',
  'target.rename',
  'target.remove',
  'target.machine',
  'target.renew',
  'doctor.run',
  'whoami.show',
] as const;

export type ActionId = (typeof ACTION_IDS)[number];

export const ACTION_COMMANDS: { readonly [K in ActionId]: ApiCommand } = {
  // The add-target wizard's Save target.
  'target.add': 'op_plan_target_add',
  'target.list': 'target_list',
  'target.show': 'target_show',
  'target.use': 'op_plan_target_use',
  'target.rename': 'op_plan_target_rename',
  'target.remove': 'op_plan_target_remove',
  // Target › Machine › Change: Apply machine.
  'target.machine': 'op_plan_target_machine',
  'target.renew': 'op_plan_target_renew',
  // The Doctor overlay (the sidebar's stethoscope, the Target screen's Run doctor).
  'doctor.run': 'op_start_doctor',
  // Settings › About › This computer.
  'whoami.show': 'whoami',
};

/**
 * Slices whose every leaf is covered: coverage.test.ts fails on a leaf still planned in one.
 * D.3 closes once its last four leaves (target add, target machine, doctor, whoami) are bound.
 */
export const CLOSED_SLICES: readonly string[] = [];
