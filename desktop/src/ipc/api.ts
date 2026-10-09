// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One typed function per Rust command (generated/commands.ts). Arguments are camelCase, which
// Tauri maps onto the Rust parameters. Every rejection is an IpcError carrying a UiError; an
// authentication refusal is also heard by onAuthRefusal, whichever command it came from.
import { type Channel, type InvokeArgs, invoke } from '@tauri-apps/api/core';
import type { AppInfo } from './generated/AppInfo';
import type { COMMANDS } from './generated/commands';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { LockState } from './generated/LockState';
import type { OpEvent } from './generated/OpEvent';
import type { OpId } from './generated/OpId';
import type { OpSummary } from './generated/OpSummary';
import type { Settings } from './generated/Settings';
import type { Subscribed } from './generated/Subscribed';
import type { SubscriptionId } from './generated/SubscriptionId';
import type { UiError } from './generated/UiError';

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
 * `error.fields.messages`. A failed gesture (`auth_failed`) or a busy prompt keeps the plan:
 * the same `opId` can be executed again.
 */
export const opExecute = (opId: OpId, onEvent: Channel<OpEvent>, password?: string) =>
  call<SubscriptionId>(
    'op_execute',
    password === undefined ? { opId, onEvent } : { opId, onEvent, password },
  );
/** The page has painted: the window, created hidden, shows. */
export const windowReady = () => call<void>('window_ready');
