// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A stand-in for the Rust side, for `vite dev` in a browser (`bun run dev:mock`) and the
// Playwright smoke: every command answers from in-memory state, behind the same lock gate.
// The app starts locked (`startup`); Unlock unlocks without asking anyone.
import type { InvokeArgs } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import type { AppInfo } from '../generated/AppInfo';
import type { AuthInfo } from '../generated/AuthInfo';
import { ALLOWED_WHILE_LOCKED } from '../generated/commands';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import { LOCK_CHANGED } from '../generated/events';
import type { LockReason } from '../generated/LockReason';
import type { LockState } from '../generated/LockState';
import type { Os } from '../generated/Os';
import type { Settings } from '../generated/Settings';
import type { Theme } from '../generated/Theme';
import type { UiError } from '../generated/UiError';

export interface MockOptions {
  readonly os?: Os;
  readonly theme?: Theme;
}

const OSES: readonly Os[] = ['windows', 'macos', 'linux'];
const THEMES: readonly Theme[] = ['system', 'light', 'dark'];

/** `?os=windows|macos|linux&theme=system|light|dark`; an unknown value throws. */
export function mockOptionsFromUrl(search: string): MockOptions {
  const params = new URLSearchParams(search);
  const pick = <T extends string>(name: string, allowed: readonly T[]): T | undefined => {
    const value = params.get(name);
    if (value === null) return undefined;
    if (!(allowed as readonly string[]).includes(value)) {
      throw new Error(`mock IPC: ?${name}=${value} is not one of ${allowed.join(', ')}`);
    }
    return value as T;
  };
  const os = pick('os', OSES);
  const theme = pick('theme', THEMES);
  return { ...(os && { os }), ...(theme && { theme }) };
}

// Rust's Settings::default().
const DEFAULT_SETTINGS: Settings = {
  version: 1,
  theme: 'dark',
  lockEnabled: true,
  lockOnStart: true,
  lockOnSleep: true,
  hello: true,
  autoLock: '10',
  refresh: '5',
  pauseHidden: true,
  osNotify: true,
  trayBadge: true,
  closeToTray: true,
};

const AUTH: Record<Os, AuthInfo> = {
  windows: {
    available: true,
    method: 'windows_hello',
    unavailable: null,
    biometricsChoice: true,
    passwordField: false,
  },
  macos: {
    available: true,
    method: 'mac_local_authentication',
    unavailable: null,
    biometricsChoice: false,
    passwordField: false,
  },
  linux: {
    available: true,
    method: 'polkit',
    unavailable: null,
    biometricsChoice: false,
    passwordField: false,
  },
};

const uiError = (code: string, message: string): UiError => ({
  code,
  message,
  help: null,
  causes: [],
  fields: {},
});

export function installMockIpc(options: MockOptions = {}): void {
  const os = options.os ?? 'macos';
  let settings: Settings = { ...DEFAULT_SETTINGS, theme: options.theme ?? DEFAULT_SETTINGS.theme };

  const autoLockMinutes = () =>
    settings.lockEnabled && settings.autoLock !== 'never' ? Number(settings.autoLock) : null;
  const stateOf = (reason: LockReason | null): LockState => ({
    locked: reason !== null,
    reason,
    sinceMs: Date.now(),
    autoLockMinutes: autoLockMinutes(),
  });
  let lock = stateOf('startup');

  const transition = async (reason: LockReason | null) => {
    lock = stateOf(reason);
    await emit(LOCK_CHANGED, lock);
    return lock;
  };

  const appInfo = (): AppInfo => ({
    os,
    desktopVersion: '0.0.0-mock',
    coreVersion: '0.0.0-mock',
    secretBackend: 'file',
    account: 'alex',
    host: 'workstation',
    auth: AUTH[os],
    testBuild: false,
    settingsNotice: null,
  });

  const notFound = (args: InvokeArgs | undefined) => {
    const opId = (args as { opId?: number } | undefined)?.opId;
    return Promise.reject(
      uiError(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND, `No plan or operation ${opId} (mock IPC).`),
    );
  };

  const handlers: Record<string, (args: InvokeArgs | undefined) => unknown> = {
    app_info: appInfo,
    settings_get: () => settings,
    settings_set: (args) => {
      settings = (args as { settings: Settings }).settings;
      lock = { ...lock, autoLockMinutes: autoLockMinutes() };
      return settings;
    },
    lock_status: () => lock,
    lock_now: () => (settings.lockEnabled ? transition('manual') : lock),
    unlock: () => (lock.locked ? transition(null) : lock),
    activity: () => null,
    quit: () => null,
    op_list: () => [],
    op_subscribe: notFound,
    op_unsubscribe: () => null,
    op_cancel: notFound,
    op_discard: () => null,
    op_execute: notFound,
    'plugin:window|set_theme': () => null,
  };

  mockWindows('main');
  mockIPC(
    (cmd, args) => {
      const handler = Object.hasOwn(handlers, cmd) ? handlers[cmd] : undefined;
      if (handler === undefined) return Promise.reject(`Command ${cmd} not found (mock IPC)`);
      // Rust's gate: plugin commands never pass it, app commands only the allowed ones.
      const gated =
        !cmd.startsWith('plugin:') && !(ALLOWED_WHILE_LOCKED as readonly string[]).includes(cmd);
      if (lock.locked && gated) {
        return Promise.reject(uiError(DESKTOP_ERROR_CODES.LOCKED, 'AppRafter is locked.'));
      }
      return handler(args);
    },
    { shouldMockEvents: true },
  );
}
