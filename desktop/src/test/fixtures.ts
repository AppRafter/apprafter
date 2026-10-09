// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The IPC answers most tests start from, with the fields a test cares about overridden.
import type { AppInfo } from '../ipc/generated/AppInfo';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import type { LockState } from '../ipc/generated/LockState';
import type { Settings } from '../ipc/generated/Settings';

export function authInfo(more: Partial<AuthInfo> = {}): AuthInfo {
  return {
    available: true,
    method: 'polkit',
    unavailable: null,
    biometricsChoice: false,
    passwordField: false,
    ...more,
  };
}

export function appInfo(more: Partial<AppInfo> = {}): AppInfo {
  return {
    os: 'linux',
    desktopVersion: '0.1.0',
    coreVersion: '0.2.80',
    secretBackend: 'file',
    account: 'alex',
    host: 'workstation',
    auth: authInfo(),
    testBuild: false,
    settingsNotice: null,
    ...more,
  };
}

/** Rust's Settings::default(), with overrides. */
export function settings(more: Partial<Settings> = {}): Settings {
  return {
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
    ...more,
  };
}

export function lockState(more: Partial<LockState> = {}): LockState {
  const locked = more.locked ?? true;
  return {
    locked,
    reason: locked ? 'startup' : null,
    sinceMs: 0,
    autoLockMinutes: 10,
    ...more,
  };
}
