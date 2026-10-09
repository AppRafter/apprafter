// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One typed function per Rust command (generated/commands.ts). Arguments are camelCase, which
// Tauri maps onto the Rust parameters. Every rejection is an IpcError carrying a UiError; an
// authentication refusal is also heard by onAuthRefusal, whichever command it came from.
import { type Channel, type InvokeArgs, invoke } from '@tauri-apps/api/core';
import type { AppInfo } from './generated/AppInfo';
import type { CatalogueSourceArg } from './generated/CatalogueSourceArg';
import type { COMMANDS } from './generated/commands';
import type { DraftId } from './generated/DraftId';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { LockState } from './generated/LockState';
import type { OpEvent } from './generated/OpEvent';
import type { OpId } from './generated/OpId';
import type { OpSummary } from './generated/OpSummary';
import type { PlanView } from './generated/PlanView';
import type { Settings } from './generated/Settings';
import type { SshKeyCandidate } from './generated/SshKeyCandidate';
import type { SshKeyInfo } from './generated/SshKeyInfo';
import type { Subscribed } from './generated/Subscribed';
import type { SubscriptionId } from './generated/SubscriptionId';
import type { TargetAddArgs } from './generated/TargetAddArgs';
import type { TargetListReport } from './generated/TargetListReport';
import type { TargetReport } from './generated/TargetReport';
import type { Theme } from './generated/Theme';
import type { ToolchainReport } from './generated/ToolchainReport';
import type { UiError } from './generated/UiError';
import type { WhoamiReport } from './generated/WhoamiReport';

/** The commands the functions below call; api.test.ts holds it equal to Rust's COMMANDS. */
export const API_COMMANDS = [
  'app_info',
  'settings_get',
  'settings_set',
  'lock_status',
  'lock_now',
  'unlock',
  'unlock_with_password',
  'activity',
  'quit',
  'op_list',
  'op_subscribe',
  'op_unsubscribe',
  'op_cancel',
  'op_discard',
  'op_execute',
  'window_ready',
  'theme_apply',
  // D.3: targets, doctor, whoami.
  'target_list',
  'target_show',
  'ssh_key_candidates',
  'ssh_key_inspect',
  'toolchain_status',
  'whoami',
  'op_start_verify_token',
  'op_start_machine_catalogue',
  'op_start_region_latencies',
  'op_start_doctor',
  'op_start_whoami',
  'op_plan_target_add',
  'op_plan_target_renew',
  'op_plan_target_use',
  'op_plan_target_rename',
  'op_plan_target_remove',
  'op_plan_target_machine',
  'target_draft_discard',
] as const satisfies readonly (typeof COMMANDS)[number][];

export type ApiCommand = (typeof API_COMMANDS)[number];

/** A command that rejected; `error` is what the UI shows and maps to an action. */
export class IpcError extends Error {
  readonly command: ApiCommand;
  readonly error: UiError;

  constructor(command: ApiCommand, error: UiError) {
    super(error.message);
    this.name = 'IpcError';
    this.command = command;
    this.error = error;
  }
}

function isUiError(value: unknown): value is UiError {
  if (typeof value !== 'object' || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    (typeof v.code === 'string' || v.code === null) &&
    typeof v.message === 'string' &&
    Array.isArray(v.causes)
  );
}

/**
 * Whatever a call or a callback rejected with, as the UiError the UI shows: an IpcError's own,
 * a UiError as it is (a command answers with one), and anything else — Tauri's own refusals (the
 * ACL, bad arguments) are text — as a message with no code.
 */
export function uiErrorOf(reason: unknown): UiError {
  if (reason instanceof IpcError) return reason.error;
  if (isUiError(reason)) return reason;
  const message = reason instanceof Error ? reason.message : String(reason);
  return { code: null, message, help: null, causes: [], fields: {} };
}

/**
 * The refusals after which what app_info says of authentication may have changed: Linux shows
 * the password field once polkit finds no agent, and stops where the OS prompts after all.
 */
const AUTH_REFUSALS: ReadonlySet<string | null> = new Set([
  DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
  DESKTOP_ERROR_CODES.AUTH_FAILED,
  DESKTOP_ERROR_CODES.AUTH_BUSY,
]);

const authRefusalListeners = new Set<(error: UiError) => void>();

/**
 * Hear every authentication refusal (`auth_unavailable`, `auth_failed`, `auth_busy`) from any
 * command, before its caller does; the returned function stops it.
 */
export function onAuthRefusal(listener: (error: UiError) => void): () => void {
  authRefusalListeners.add(listener);
  return () => {
    authRefusalListeners.delete(listener);
  };
}

async function call<T>(command: ApiCommand, args?: InvokeArgs): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (reason) {
    const error = uiErrorOf(reason);
    if (AUTH_REFUSALS.has(error.code)) {
      for (const listener of authRefusalListeners) {
        try {
          listener(error);
        } catch (failure) {
          console.error('an authentication refusal listener failed:', failure);
        }
      }
    }
    throw new IpcError(command, error);
  }
}

export const appInfo = () => call<AppInfo>('app_info');
export const settingsGet = () => call<Settings>('settings_get');
/** Save and apply; the settings now in use. */
export const settingsSet = (settings: Settings) => call<Settings>('settings_set', { settings });
export const lockStatus = () => call<LockState>('lock_status');
/** Lock now; the resulting state (unlocked when the lock is off). */
export const lockNow = () => call<LockState>('lock_now');
/** Rust asks the OS for the owner; the resulting state. */
export const unlock = () => call<LockState>('unlock');
/**
 * The lock screen's own password field, shown where the OS cannot prompt
 * (`AuthInfo.passwordField`): Rust checks the password as `unlock` asks the OS; the resulting
 * state. A refusal may carry what the OS said (PAM's messages) in `error.fields.messages`.
 */
export const unlockWithPassword = (password: string) =>
  call<LockState>('unlock_with_password', { password });
/** The owner did something: the idle time starts again. */
export const activity = () => call<void>('activity');
export const quit = () => call<void>('quit');
/** Running and recently ended operations, the latest started first. */
export const opList = () => call<OpSummary[]>('op_list');
/** The events so far, and every later one on `onEvent`. */
export const opSubscribe = (opId: OpId, onEvent: Channel<OpEvent>) =>
  call<Subscribed>('op_subscribe', { opId, onEvent });
export const opUnsubscribe = (opId: OpId, subscription: SubscriptionId) =>
  call<void>('op_unsubscribe', { opId, subscription });
export const opCancel = (opId: OpId) => call<void>('op_cancel', { opId });
export const opDiscard = (opId: OpId) => call<void>('op_discard', { opId });
/**
 * Run a confirmed plan; `onEvent` follows it. When the plan needs the owner's gesture Rust asks
 * the OS — or, given `password` (the confirm dialog's own field, shown where
 * `AuthInfo.passwordField`), checks it instead; a refusal may then carry what the OS said in
 * `error.fields.messages`. A failed gesture (`auth_failed`), a busy prompt, a gesture asked the
 * way that is not there (`auth_unavailable` with `no_agent`, `use_password_field` or
 * `use_system_prompt`) and an
 * expired password (`password_expired`) keep the plan: the same `opId` can be executed again.
 */
export const opExecute = (opId: OpId, onEvent: Channel<OpEvent>, password?: string) =>
  call<SubscriptionId>(
    'op_execute',
    password === undefined ? { opId, onEvent } : { opId, onEvent, password },
  );
/** The page has painted: the window, created hidden, shows. */
export const windowReady = () => call<void>('window_ready');
/**
 * The native window follows the theme setting: Light and Dark as they are; System left to the
 * OS on macOS and Windows, and on Linux resolved by Rust from the desktop's colour scheme and
 * followed while it stays System. Answered while locked.
 */
export const themeApply = (theme: Theme) => call<void>('theme_apply', { theme });

// D.3: targets, doctor, whoami. A read (`opStart*`) answers with the id of an operation to follow
// (`opSubscribe`) and cancel (`opCancel`); a plan (`opPlanTarget*`) with the view to confirm and
// then execute (`opExecute`). Every target is named: none of these acts on the CLI's default.

/** Every target in the store, the unreadable ones listed apart. */
export const targetList = () => call<TargetListReport>('target_list');
/** One target in full; an unknown name is `TARGET_NOT_FOUND` with the names there are. */
export const targetShow = (name: string) => call<TargetReport>('target_show', { name });
/** The public keys under `~/.ssh` the key picker offers. */
export const sshKeyCandidates = () => call<SshKeyCandidate[]>('ssh_key_candidates');
export const sshKeyInspect = (path: string) => call<SshKeyInfo>('ssh_key_inspect', { path });
/** Each tool the app runs and where it was looked for. */
export const toolchainStatus = () => call<ToolchainReport>('toolchain_status');
/** The About row: no ping (its verification is `skipped`); `opStartWhoami` verifies. */
export const whoami = () => call<WhoamiReport>('whoami');
/** The token crosses IPC here, once; the read's result (TokenVerified) names its draft. */
export const opStartVerifyToken = (provider: string, token: string) =>
  call<OpId>('op_start_verify_token', { provider, token });
/** The regions and machines a picker offers, read with a draft's token or a stored target's. */
export const opStartMachineCatalogue = (source: CatalogueSourceArg) =>
  call<OpId>('op_start_machine_catalogue', { source });
export const opStartRegionLatencies = (regions: readonly string[]) =>
  call<OpId>('op_start_region_latencies', { regions });
/** Doctor on the target called `target`. */
export const opStartDoctor = (target: string) => call<OpId>('op_start_doctor', { target });
/** whoami with a ping of the CLI default's stored token. */
export const opStartWhoami = () => call<OpId>('op_start_whoami');
/** Plan adding a target with a verified token's draft (bounded); the plan takes the draft. */
export const opPlanTargetAdd = (args: TargetAddArgs) =>
  call<PlanView>('op_plan_target_add', { args });
/**
 * Plan renewing a target's token (bounded); the token crosses IPC here, once per attempt. With
 * `sshKey` (a path, as `sshKeyInspect` answers it) the plan also sets the target's SSH key: the
 * core saves a key only with a new token (`target add <name> --renew --ssh-key <path>`).
 */
export const opPlanTargetRenew = (name: string, token: string, sshKey: string | null) =>
  call<PlanView>('op_plan_target_renew', { name, token, sshKey });
/** Plan making `name` the CLI's default (reversible: execute it at once). */
export const opPlanTargetUse = (name: string) => call<PlanView>('op_plan_target_use', { name });
export const opPlanTargetRename = (from: string, to: string) =>
  call<PlanView>('op_plan_target_rename', { from, to });
/** Plan removing `name` from this computer (destructive: the OS gesture runs in `opExecute`). */
export const opPlanTargetRemove = (name: string) =>
  call<PlanView>('op_plan_target_remove', { name });
/** Plan changing the machine of `name` (bounded); a provisioned target is refused. */
export const opPlanTargetMachine = (name: string, sku: string, region: string | null) =>
  call<PlanView>('op_plan_target_machine', { name, sku, region });
/** The add wizard closed: its draft goes. An unknown draft is no error. */
export const targetDraftDiscard = (draftId: DraftId) =>
  call<void>('target_draft_discard', { draftId });
