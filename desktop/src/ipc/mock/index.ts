// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A stand-in for the Rust side, for `vite dev` in a browser (`bun run dev:mock`) and the
// Playwright smoke: every command answers from in-memory state, behind the same lock gate.
// The app starts locked (`startup`). As Rust, by route (`?auth=`): by default the OS prompts
// itself, so Unlock unlocks without asking anyone and the password field is refused
// (`use_system_prompt`); with `?auth=pam` (Linux's PAM route) the field is the way — it unlocks
// with MOCK_PASSWORD and refuses anything else as PAM would, saying MOCK_PAM_SAYS, behind Rust's
// back-off (MOCK_BACKOFF: too many wrong passwords, and every try is turned away for a while,
// each answer saying how long as `retryInMs`) — and Unlock finds no polkit agent (`no_agent`).
// What the session tells the app (`?session=`): by default both its locks and its sleeps, as a
// desktop session does; or one half, or nothing at all (no bus, as in WSL or a container), for
// the settings' lock-on-sleep row. Operations run on ops.ts's engine (Rust's OperationManager):
// a destructive plan asks the gesture the way unlocking does, by the same route and back-off.
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
import type { SessionEvents } from '../generated/SessionEvents';
import type { Settings } from '../generated/Settings';
import type { Theme } from '../generated/Theme';
import type { UiError } from '../generated/UiError';
import type { UnavailableReason } from '../generated/UnavailableReason';
import { createMockOps, type Handler, type MockOps } from './ops';

export { MOCK_TARGETS } from './fixtures';

/** The one password the mock's password field accepts: a demo value, nobody's password. */
export const MOCK_PASSWORD = 'apprafter';

/** What the mock's password field says with a wrong password, as PAM would. */
export const MOCK_PAM_SAYS = 'Authentication failure';

/**
 * Rust's back-off (`Backoff` in desktop/os-auth/src/outcome.rs; index.test.ts holds the two
 * equal): `failures` wrong passwords start a refusal of `refusalMs`, counted on the monotonic
 * clock (`performance.now()`, which the Playwright smoke fakes).
 */
export const MOCK_BACKOFF: { readonly failures: number; readonly refusalMs: number } = {
  failures: 3,
  refusalMs: 30_000,
};

/** How the mock OS verifies the owner: its own prompt, or Linux's PAM route (the app's field). */
export type MockAuth = 'os' | 'pam';

/** Which of the session's signals reach the app: both, one of them, or none. */
export type MockSession = 'both' | 'lock' | 'sleep' | 'none';

export interface MockOptions {
  readonly os?: Os;
  readonly theme?: Theme;
  /** `pam`: Linux's PAM route, whatever `os` says. */
  readonly auth?: MockAuth;
  readonly session?: MockSession;
  /**
   * How long a mock operation takes before it reports, in milliseconds: long enough in dev
   * mode (150 by default) to see it running; tests pass 0.
   */
  readonly opDelayMs?: number;
}

const OSES: readonly Os[] = ['windows', 'macos', 'linux'];
const THEMES: readonly Theme[] = ['system', 'light', 'dark'];
const AUTHS: readonly MockAuth[] = ['os', 'pam'];
const SESSIONS: readonly MockSession[] = ['both', 'lock', 'sleep', 'none'];

const SESSION_EVENTS: Record<MockSession, SessionEvents> = {
  both: { lock: true, sleep: true },
  lock: { lock: true, sleep: false },
  sleep: { lock: false, sleep: true },
  none: { lock: false, sleep: false },
};

/**
 * `?os=windows|macos|linux&theme=system|light|dark&auth=os|pam&session=both|lock|sleep|none`;
 * an unknown value throws.
 */
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
  const session = pick('session', SESSIONS);
  return {
    ...(os && { os }),
    ...(theme && { theme }),
    ...(auth && { auth }),
    ...(session && { session }),
  };
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
const unavailable = (reason: UnavailableReason): UiError => ({
  ...uiError(
    DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
    `device-owner authentication is unavailable here (${reason})`,
  ),
  fields: { reason },
});

/** The engine of the last installMockIpc. */
let current: MockOps | null = null;

/** The engine of the last installMockIpc; tests register plans and reads through it. */
export function mockOps(): MockOps {
  if (current === null) throw new Error('mock IPC: installMockIpc() has not run');
  return current;
}

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
    // Rust's lock hook, on every lock and unlock: plans dropped, reads cancelled, every
    // subscription ended.
    engine.transition();
    seq += 1;
    lock = stateOf(reason);
    await emit(LOCK_CHANGED, lock);
    return lock;
  };

  // Rust's back-off on the password field: wrong passwords since the last refusal or success,
  // and the monotonic reading the current refusal ends at.
  let failures = 0;
  let refusedUntil = 0;
  const failed = (fields: UiError['fields']) =>
    Promise.reject({
      ...uiError(DESKTOP_ERROR_CODES.AUTH_FAILED, 'authentication failed'),
      fields,
    });
  /** The password field's check, for unlocking and for a destructive plan's gesture alike. */
  const verifyPassword = (password: unknown): Promise<void> => {
    const now = performance.now();
    // Turned away unchecked: nothing was said, and the right password does not help.
    if (now < refusedUntil) return failed({ exhausted: true, retryInMs: refusedUntil - now });
    if (password === MOCK_PASSWORD) {
      failures = 0;
      return Promise.resolve();
    }
    failures += 1;
    if (failures < MOCK_BACKOFF.failures) {
      return failed({ exhausted: false, messages: [MOCK_PAM_SAYS] });
    }
    failures = 0;
    refusedUntil = now + MOCK_BACKOFF.refusalMs;
    return failed({
      exhausted: true,
      retryInMs: MOCK_BACKOFF.refusalMs,
      messages: [MOCK_PAM_SAYS],
    });
  };

  const appInfo = (): AppInfo => ({
    os,
    desktopVersion: '0.0.0-mock',
    coreVersion: '0.0.0-mock',
    secretBackend: 'file',
    account: 'alex',
    host: 'workstation',
    auth,
    sessionEvents: SESSION_EVENTS[options.session ?? 'both'],
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

  // A destructive plan's gesture, by the unlock's route: on the PAM route the field (a prompt
  // finds no agent), elsewhere the OS's own prompt (the field is refused). Each refusal here is
  // one after which Rust keeps the plan.
  const engine = createMockOps({
    delayMs: options.opDelayMs ?? 150,
    gesture: (password) => {
      if (auth.passwordField) {
        return password === undefined
          ? Promise.reject(unavailable('no_agent'))
          : verifyPassword(password);
      }
      return password === undefined
        ? Promise.resolve()
        : Promise.reject(unavailable('use_system_prompt'));
    },
  });
  current = engine;

  const handlers: Record<string, Handler> = {
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
      if (!auth.passwordField) return Promise.reject(unavailable('use_system_prompt'));
      return verifyPassword((args as { password?: unknown } | undefined)?.password).then(() =>
        transition(null),
      );
    },
    activity: () => null,
    quit: () => null,
    ...engine.handlers,
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
