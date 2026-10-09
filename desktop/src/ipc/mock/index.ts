// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A stand-in for the Rust side, for `vite dev` in a browser (`bun run dev:mock`) and the
// Playwright smoke: every command answers from in-memory state, behind the same lock gate.
// The app starts locked (`startup`). As Rust, by route (`?auth=`): by default the OS prompts
// itself, so Unlock unlocks without asking anyone and the password field is refused
// (`not_permitted_here`); with `?auth=pam` (Linux's PAM route) the field is the way — it unlocks
// with MOCK_PASSWORD and refuses anything else as PAM would, saying MOCK_PAM_SAYS — and Unlock
// finds no polkit agent (`no_agent`).
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

export { MOCK_TARGETS } from './fixtures';

/** The one password the mock's password field accepts: a demo value, nobody's password. */
export const MOCK_PASSWORD = 'apprafter';

/** What the mock's password field says with a wrong password, as PAM would. */
export const MOCK_PAM_SAYS = 'Authentication failure';

/** How the mock OS verifies the owner: its own prompt, or Linux's PAM route (the app's field). */
export type MockAuth = 'os' | 'pam';

export interface MockOptions {
  readonly os?: Os;
  readonly theme?: Theme;
  /** `pam`: Linux's PAM route, whatever `os` says. */
  readonly auth?: MockAuth;
}

const OSES: readonly Os[] = ['windows', 'macos', 'linux'];
const THEMES: readonly Theme[] = ['system', 'light', 'dark'];
const AUTHS: readonly MockAuth[] = ['os', 'pam'];

/** `?os=windows|macos|linux&theme=system|light|dark&auth=os|pam`; an unknown value throws. */
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
  const auth = pick('auth', AUTHS);
  return { ...(os && { os }), ...(theme && { theme }), ...(auth && { auth }) };
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

/** Linux's PAM route: polkit cannot prompt here, so the app shows its own field. */
const PAM: AuthInfo = {
  available: true,
  method: 'pam',
  unavailable: null,
  biometricsChoice: false,
  passwordField: true,
};

const uiError = (code: string, message: string): UiError => ({
  code,
  message,
  help: null,
  causes: [],
  fields: {},
});

/** Rust's `auth_unavailable` for `reason`. */
const unavailable = (reason: string): UiError => ({
  ...uiError(
    DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
    `device-owner authentication is unavailable here (${reason})`,
  ),
  fields: { reason },
});

export function installMockIpc(options: MockOptions = {}): void {
  const os = options.os ?? 'macos';
  const auth = options.auth === 'pam' ? PAM : AUTH[os];
  let settings: Settings = { ...DEFAULT_SETTINGS, theme: options.theme ?? DEFAULT_SETTINGS.theme };

  const autoLockMinutes = () =>
    settings.lockEnabled && settings.autoLock !== 'never' ? Number(settings.autoLock) : null;
  // As Rust numbers them: 0 at start, one more on every lock and unlock.
  let seq = 0;
  const stateOf = (reason: LockReason | null): LockState => ({
    locked: reason !== null,
    reason,
    sinceMs: Date.now(),
    autoLockMinutes: autoLockMinutes(),
    seq,
  });
  let lock = stateOf('startup');

  const transition = async (reason: LockReason | null) => {
    seq += 1;
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
    auth,
    // A desktop session: the OS reports both its locks and its sleeps.
    sessionEvents: { lock: true, sleep: true },
    testBuild: false,
    settingsNotice: null,
  });

  // The window, as far as the title bar can tell: maximized or not, and a resize event.
  let maximized = false;
  const toggleMaximize = async () => {
    maximized = !maximized;
    await emit('tauri://resize', { width: 1280, height: 800 });
    return null;
  };

  const notFound = (args: InvokeArgs | undefined) => {
    const opId = (args as { opId?: number } | undefined)?.opId;
    return Promise.reject(
      uiError(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND, `No plan or operation ${opId} (mock IPC).`),
    );
  };

  const handlers: Record<string, (args: InvokeArgs | undefined) => unknown> = {
    app_info: appInfo,
    settings_get: () => settings,
    settings_set: async (args) => {
      settings = (args as { settings: Settings }).settings;
      // As Rust: a new idle time is the same lock reported anew, numbered and emitted.
      if (autoLockMinutes() !== lock.autoLockMinutes) {
        seq += 1;
        lock = { ...lock, autoLockMinutes: autoLockMinutes(), seq };
        await emit(LOCK_CHANGED, lock);
      }
      return settings;
    },
    lock_status: () => lock,
    lock_now: () => (settings.lockEnabled ? transition('manual') : lock),
    // As Rust: unlocked already, nothing is asked or checked.
    unlock: () => {
      if (!lock.locked) return lock;
      if (auth.passwordField) return Promise.reject(unavailable('no_agent'));
      return transition(null);
    },
    unlock_with_password: (args) => {
      if (!lock.locked) return lock;
      if (!auth.passwordField) return Promise.reject(unavailable('not_permitted_here'));
      if ((args as { password?: unknown } | undefined)?.password === MOCK_PASSWORD) {
        return transition(null);
      }
      return Promise.reject({
        ...uiError(DESKTOP_ERROR_CODES.AUTH_FAILED, 'authentication failed'),
        fields: { exhausted: false, messages: [MOCK_PAM_SAYS] },
      });
    },
    activity: () => null,
    quit: () => null,
    op_list: () => [],
    op_subscribe: notFound,
    op_unsubscribe: () => null,
    op_cancel: notFound,
    op_discard: () => null,
    op_execute: notFound,
    window_ready: () => null,
    'plugin:window|set_theme': () => null,
    'plugin:window|minimize': () => null,
    'plugin:window|toggle_maximize': toggleMaximize,
    'plugin:window|internal_toggle_maximize': toggleMaximize,
    'plugin:window|is_maximized': () => maximized,
    'plugin:window|start_dragging': () => null,
    // A browser has no opener: a new tab stands in for it.
    'plugin:opener|open_url': (args) => {
      window.open(String((args as { url: string }).url), '_blank', 'noopener');
      return null;
    },
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
