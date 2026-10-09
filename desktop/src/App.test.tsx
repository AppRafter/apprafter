// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen } from '@testing-library/react';
import { App } from './App';
import { DESKTOP_ERROR_CODES } from './ipc/generated/errors';
import type { LockState } from './ipc/generated/LockState';
import type { Settings } from './ipc/generated/Settings';
import { resetOperations } from './ipc/operations';
import { appInfo, lockState, settings } from './test/fixtures';

let calls: { cmd: string; args: unknown }[];
let appInfoAnswer: () => unknown;
let status: LockState;
let stored: Settings;

beforeEach(() => {
  calls = [];
  appInfoAnswer = () => appInfo();
  status = lockState({ locked: false });
  stored = settings();
  mockWindows('main');
  mockIPC(
    (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === 'app_info') return appInfoAnswer();
      if (cmd === 'lock_status') return status;
      if (cmd === 'settings_get') return stored;
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
  delete document.documentElement.dataset.theme;
});

const paint = () => act(() => new Promise((resolve) => setTimeout(resolve, 50)));
const count = (cmd: string) => calls.filter((c) => c.cmd === cmd).length;

test('unlocked, the app opens on the Targets view and shows its window once', async () => {
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
  await paint();
  expect(count('app_info')).toBe(1);
  expect(count('window_ready')).toBe(1);
});

test('locked at start, the app is the lock screen, in the chosen theme', async () => {
  status = lockState({ reason: 'startup' });
  stored = settings({ theme: 'light' });
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
  expect(screen.queryByRole('heading', { name: 'Open a cluster' })).toBeNull();
  await paint();
  expect(document.documentElement.dataset.theme).toBe('light');
  expect(calls.find((c) => c.cmd === 'plugin:window|set_theme')?.args).toEqual({
    label: 'main',
    value: 'light',
  });
});

test('when app_info fails, the error shows — and the window still does', async () => {
  appInfoAnswer = () =>
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
  expect(count('window_ready')).toBe(1);
});
