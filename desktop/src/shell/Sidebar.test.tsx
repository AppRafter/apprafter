// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { AppInfo } from '../ipc/generated/AppInfo';
import type { OpSummary } from '../ipc/generated/OpSummary';
import { refreshList, resetOperations } from '../ipc/operations';
import type { TargetTab } from '../state/session';
import { Sidebar, type SidebarProps } from './Sidebar';
import { ViewFrame } from './ViewFrame';

const INFO: AppInfo = {
  os: 'linux',
  desktopVersion: '0.1.0',
  coreVersion: '0.2.80',
  secretBackend: 'file',
  account: 'alex',
  host: 'workstation',
  auth: {
    available: true,
    method: 'polkit',
    unavailable: null,
    biometricsChoice: false,
    passwordField: false,
  },
  sessionEvents: true,
  testBuild: false,
  settingsNotice: null,
};

const TAB: TargetTab = { key: 'a', target: 'prod-eu', section: 'apps' };

let calls: { cmd: string; args: unknown }[];
let summaries: OpSummary[];

beforeEach(() => {
  calls = [];
  summaries = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return cmd === 'op_list' ? summaries : null;
  });
});

// Unmount first: a reset publishes, and a mounted indicator would re-render outside act().
afterEach(() => {
  cleanup();
  resetOperations();
  clearMocks();
});

function sidebar(more: Partial<SidebarProps> = {}) {
  const props: SidebarProps = {
    os: 'linux',
    tab: TAB,
    info: INFO,
    onNavigate: mock(),
    onShowTargets: mock(),
    onLock: mock(),
    ...more,
  };
  render(
    <ViewFrame>
      <Sidebar {...props} />
    </ViewFrame>,
  );
  return { user: userEvent.setup(), ...props };
}

describe('Sidebar', () => {
  test('a target tab shows its name and both navs; the current section is marked', () => {
    sidebar();
    expect(screen.getByText('prod-eu')).toBeDefined();
    const current = screen.getByRole('button', { name: 'Applications' });
    expect(current.getAttribute('aria-current')).toBe('page');
    expect(screen.getByRole('button', { name: 'Overview' }).hasAttribute('aria-current')).toBe(
      false,
    );
    expect(screen.getByRole('button', { name: 'Target' })).toBeDefined();
  });

  test('a nav item navigates', async () => {
    const { user, onNavigate } = sidebar();
    await user.click(screen.getByRole('button', { name: 'Backups' }));
    expect(onNavigate).toHaveBeenCalledWith('backups');
  });

  test('on the Targets view only the footer renders', () => {
    sidebar({ tab: null });
    expect(screen.queryByRole('button', { name: 'Overview' })).toBeNull();
    expect(screen.getByRole('button', { name: /Targets/ })).toBeDefined();
  });

  test('the footer: Targets and Lock with per-OS hints, Settings only when there is one', async () => {
    const onSettings = mock();
    const { user, onShowTargets, onLock } = sidebar({ os: 'macos', onSettings });
    await user.click(screen.getByRole('button', { name: 'Targets ⌘T' }));
    await user.click(screen.getByRole('button', { name: 'Lock ⌘L' }));
    await user.click(screen.getByRole('button', { name: 'Settings ⌘,' }));
    expect(onShowTargets).toHaveBeenCalledTimes(1);
    expect(onLock).toHaveBeenCalledTimes(1);
    expect(onSettings).toHaveBeenCalledTimes(1);
  });

  test('no Settings entry without a dialog to open', () => {
    sidebar();
    expect(screen.queryByRole('button', { name: /Settings/ })).toBeNull();
  });

  test('the links open in the browser through the opener; the version is the app info', async () => {
    const { user } = sidebar();
    await user.click(screen.getByRole('button', { name: 'GitHub' }));
    await user.click(screen.getByRole('button', { name: 'Website' }));
    await user.click(screen.getByRole('button', { name: 'Docs' }));
    expect(
      calls
        .filter((c) => c.cmd === 'plugin:opener|open_url')
        .map((c) => (c.args as { url: string }).url),
    ).toEqual([
      'https://github.com/AppRafter/apprafter',
      'https://apprafter.dev',
      'https://docs.apprafter.dev',
    ]);
    expect(screen.getByText('v0.1.0')).toBeDefined();
  });

  test('"N running" counts the running operations and opens a sheet that cancels them', async () => {
    summaries = [
      { opId: 1, title: 'Upgrade platform', target: 'prod-eu', state: 'running', startedAtMs: 2 },
      { opId: 2, title: 'Back up now', target: 'staging', state: 'running', startedAtMs: 1 },
      { opId: 3, title: 'Remove target', target: 'lab', state: 'finished', startedAtMs: 0 },
    ];
    const { user } = sidebar();
    expect(screen.queryByRole('button', { name: /running/ })).toBeNull();
    await act(() => refreshList());
    await user.click(screen.getByRole('button', { name: '2 running' }));
    const sheet = screen.getByRole('dialog', { name: 'Running operations' });
    expect(within(sheet).getByText('Upgrade platform')).toBeDefined();
    expect(within(sheet).queryByText('Remove target')).toBeNull();
    await user.click(within(sheet).getByRole('button', { name: 'Cancel Back up now' }));
    expect(calls.filter((c) => c.cmd === 'op_cancel').map((c) => c.args)).toEqual([{ opId: 2 }]);
  });
});
