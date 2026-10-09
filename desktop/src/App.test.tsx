// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen } from '@testing-library/react';
import { App } from './App';
import { DESKTOP_ERROR_CODES } from './ipc/generated/errors';
import { resetOperations } from './ipc/operations';

const INFO = {
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
  testBuild: false,
  settingsNotice: null,
};

let calls: string[];
let appInfo: () => unknown;

beforeEach(() => {
  calls = [];
  appInfo = () => INFO;
  mockWindows('main');
  mockIPC(
    (cmd) => {
      calls.push(cmd);
      if (cmd === 'app_info') return appInfo();
      if (cmd === 'op_list') return [];
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

const paint = () => act(() => new Promise((resolve) => setTimeout(resolve, 50)));

test('the app reads app_info, opens on the Targets view, and shows its window once', async () => {
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
  await paint();
  expect(calls.filter((c) => c === 'app_info')).toHaveLength(1);
  expect(calls.filter((c) => c === 'window_ready')).toHaveLength(1);
});

test('when app_info fails, the error shows — and the window still does', async () => {
  appInfo = () =>
    Promise.reject({
      code: DESKTOP_ERROR_CODES.INTERNAL,
      message: 'the shell has not started yet',
      help: null,
      causes: [],
      fields: {},
    });
  render(<App />);
  expect((await screen.findByRole('alert')).textContent).toContain('the shell has not started yet');
  await paint();
  expect(calls).toContain('window_ready');
});
