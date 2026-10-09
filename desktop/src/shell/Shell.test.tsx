// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider } from '../components/Toast';
import type { AppInfo } from '../ipc/generated/AppInfo';
import type { OpSummary } from '../ipc/generated/OpSummary';
import { MOCK_TARGETS } from '../ipc/mock/fixtures';
import { refreshList, resetOperations } from '../ipc/operations';
import { TargetsSource } from '../screens/targets/targets';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { Shell } from './Shell';

const INFO: AppInfo = {
  os: 'windows',
  desktopVersion: '0.1.0',
  coreVersion: '0.2.80',
  secretBackend: 'file',
  account: 'alex',
  host: 'workstation',
  auth: {
    available: true,
    method: 'windows_hello',
    unavailable: null,
    biometricsChoice: true,
    passwordField: false,
  },
  testBuild: false,
  settingsNotice: null,
};

let calls: string[];
let summaries: OpSummary[];

beforeEach(() => {
  calls = [];
  summaries = [];
  mockWindows('main');
  mockIPC(
    (cmd) => {
      calls.push(cmd);
      if (cmd === 'op_list') return summaries;
      if (cmd === 'plugin:window|is_maximized') return false;
      return null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(async () => {
  cleanup();
  await new Promise((resolve) => setTimeout(resolve, 0));
  resetOperations();
  clearMocks();
});

function shell() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={INFO}>
        <TargetsSource value={MOCK_TARGETS}>
          <ToastProvider>
            <Shell />
          </ToastProvider>
        </TargetsSource>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}

const tab = (name: string | RegExp) => screen.getByRole('tab', { name });
const pageTitle = () => screen.getByRole('heading', { level: 1 }).textContent;

describe('Shell', () => {
  test('starts on the Targets view; a card opens its target in a tab on Overview', async () => {
    const user = shell();
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    await user.click(screen.getByRole('button', { name: /prod-eu/ }));
    expect(tab('prod-eu').getAttribute('aria-selected')).toBe('true');
    expect(pageTitle()).toBe('Overview');
    expect(screen.queryByRole('heading', { name: 'Open a cluster' })).toBeNull();
  });

  test('each tab keeps its own section across switches', async () => {
    const user = shell();
    await user.click(screen.getByRole('button', { name: /prod-eu/ }));
    await user.click(screen.getByRole('button', { name: 'Backups' }));
    expect(pageTitle()).toBe('Backups');
    await user.click(screen.getByRole('button', { name: 'Open a cluster' }));
    await user.click(screen.getByRole('button', { name: /staging/ }));
    expect(pageTitle()).toBe('Overview');
    await user.click(tab('prod-eu'));
    expect(pageTitle()).toBe('Backups');
  });

  test('Ctrl+T shows the Targets view; Ctrl+L locks', async () => {
    const user = shell();
    await user.click(screen.getByRole('button', { name: /prod-eu/ }));
    await user.keyboard('{Control>}t{/Control}');
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    expect(tab('prod-eu').getAttribute('aria-selected')).toBe('false');
    await user.keyboard('{Control>}l{/Control}');
    expect(calls).toContain('lock_now');
  });

  test('closing the shown tab shows its neighbour, and the last one the Targets view', async () => {
    const user = shell();
    await user.click(screen.getByRole('button', { name: /prod-eu/ }));
    await user.click(screen.getByRole('button', { name: 'Open a cluster' }));
    await user.click(screen.getByRole('button', { name: /staging/ }));
    await user.click(screen.getByRole('button', { name: 'Close staging' }));
    expect(tab('prod-eu').getAttribute('aria-selected')).toBe('true');
    await user.click(screen.getByRole('button', { name: 'Close prod-eu' }));
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    expect(screen.queryAllByRole('tab')).toHaveLength(0);
  });

  test("a tab's overlay hides with its tab, and is back when the tab is", async () => {
    summaries = [
      { opId: 1, title: 'Upgrade platform', target: 'prod-eu', state: 'running', startedAtMs: 1 },
    ];
    const user = shell();
    await act(() => refreshList());
    await user.click(screen.getByRole('button', { name: /prod-eu/ }));
    await user.click(screen.getByRole('button', { name: '1 running' }));
    expect(screen.getByRole('dialog', { name: 'Running operations' })).toBeDefined();
    await user.keyboard('{Control>}t{/Control}');
    expect(screen.queryByRole('dialog', { name: 'Running operations' })).toBeNull();
    await user.click(tab(/prod-eu/));
    expect(
      within(screen.getByRole('dialog', { name: 'Running operations' })).getByText(
        'Upgrade platform',
      ),
    ).toBeDefined();
  });
});
