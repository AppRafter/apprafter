// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The IPC answers most tests start from, with the fields a test cares about overridden.
import type { AppInfo } from '../ipc/generated/AppInfo';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import type { LockState } from '../ipc/generated/LockState';
import type { PlanView } from '../ipc/generated/PlanView';
import type { ProvisionedServer } from '../ipc/generated/ProvisionedServer';
import type { ProvisionedState } from '../ipc/generated/ProvisionedState';
import type { Settings } from '../ipc/generated/Settings';
import type { TargetReport } from '../ipc/generated/TargetReport';
import type { TargetSummary } from '../ipc/generated/TargetSummary';

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
    sessionEvents: { lock: true, sleep: true },
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
    seq: 0,
    ...more,
  };
}

/** One row of target_list: prod-eu, the CLI default, on Hetzner Cloud in nbg1, Team tier. */
export function targetSummary(more: Partial<TargetSummary> = {}): TargetSummary {
  return {
    name: 'prod-eu',
    provider: 'hetzner-cloud',
    region: 'nbg1',
    serverType: 'cx22',
    defaultTier: 'team',
    tierLevel: 2,
    isCliDefault: true,
    ...more,
  };
}

/** target_show for targetSummary()'s target: a key, a stored token, no server yet. */
export function targetReport(more: Partial<TargetReport> = {}): TargetReport {
  return {
    name: 'prod-eu',
    isCliDefault: true,
    provider: 'hetzner-cloud',
    region: 'nbg1',
    serverType: 'cx22',
    defaultTier: 'team',
    tierLevel: 2,
    clusterName: null,
    sshKey: {
      path: '/home/alex/.ssh/id_ed25519.pub',
      display: '~/.ssh/id_ed25519.pub',
      exists: true,
      algo: 'ssh-ed25519',
    },
    token: { set: true, chars: 64 },
    configFile: '~/.config/apprafter/targets/prod-eu/config.yaml',
    credentialsFile: '~/.config/apprafter/targets/prod-eu/credentials.yaml',
    provisioned: { status: 'not_provisioned' },
    ...more,
  };
}

/** A target's local state recording its server, prod-eu-1. */
export function provisioned(more: Partial<ProvisionedServer> = {}): ProvisionedState {
  return {
    status: 'provisioned',
    server: { serverId: 4711, serverName: 'prod-eu-1', serverType: 'cpx22', ...more },
  };
}

/** A bounded plan for prod-eu with one change, as an op_plan_target_* command answers. */
export function planView(more: Partial<PlanView> = {}): PlanView {
  return {
    opId: 7,
    class: 'bounded',
    title: 'Rename prod-eu',
    changes: [
      { kind: 'Target', object: 'prod-eu', action: 'rename', detail: 'prod-eu → prod-eu-2' },
    ],
    target: 'prod-eu',
    expiresAtMs: 0,
    ...more,
  };
}
