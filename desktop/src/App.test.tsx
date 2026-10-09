// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { App } from './App';
import { DESKTOP_ERROR_CODES } from './ipc/generated/errors';
import { LOCK_CHANGED, QUITTING } from './ipc/generated/events';
import type { LockState } from './ipc/generated/LockState';
import type { Quitting } from './ipc/generated/Quitting';
import type { Settings } from './ipc/generated/Settings';
import { resetOperations } from './ipc/operations';
import { appInfo, authInfo, lockState, settings } from './test/fixtures';

let calls: { cmd: string; args: unknown }[];
let appInfoAnswer: () => unknown;
let lockAnswer: () => unknown;
let settingsAnswer: () => unknown;
/** The page theme when the page said it was ready. */
let themeAtReveal: string | undefined;

beforeEach(() => {
  calls = [];
  appInfoAnswer = () => appInfo();
  lockAnswer = () => lockState({ locked: false });
  settingsAnswer = () => settings();
  themeAtReveal = undefined;
  mockWindows('main');
  mockIPC(
    (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === 'app_info') return appInfoAnswer();
      if (cmd === 'lock_status') return lockAnswer();
      if (cmd === 'settings_get') return settingsAnswer();
      if (cmd === 'op_list') return [];
      if (cmd === 'window_ready') themeAtReveal = document.documentElement.dataset.theme;
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
/** Long enough for anything the page would do on its own: answers, effects, their timers. */
const idle = async () => {
  for (let turn = 0; turn < 4; turn += 1) await paint();
};
const count = (cmd: string) => calls.filter((c) => c.cmd === cmd).length;

function held<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => (resolve = res));
  return { promise, resolve };
}

const refusal = (message: string) =>
  Promise.reject({
    code: DESKTOP_ERROR_CODES.INTERNAL,
    message,
    help: null,
    causes: [],
    fields: {},
  });

test('unlocked, the app opens on the Targets view and shows its window once', async () => {
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
  await paint();
  expect(count('app_info')).toBe(1);
  expect(count('window_ready')).toBe(1);
});

test('locked at start, the app is the lock screen, in the chosen theme', async () => {
  lockAnswer = () => lockState({ reason: 'startup' });
  settingsAnswer = () => settings({ theme: 'light' });
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

test('the window shows without an animation frame: WebKitGTK runs none while it is hidden', async () => {
  const frame = globalThis.requestAnimationFrame;
  globalThis.requestAnimationFrame = () => 0;
  try {
    render(<App />);
    expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    await paint();
    expect(count('window_ready')).toBe(1);
  } finally {
    globalThis.requestAnimationFrame = frame;
  }
});

test('the window shows only once there is a first screen to show', async () => {
  const lock = held<LockState>();
  lockAnswer = () => lock.promise;
  render(<App />);
  await idle();
  expect(count('lock_status')).toBe(1);
  expect(count('window_ready')).toBe(0);
  await act(async () => lock.resolve(lockState({ locked: false })));
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
  await paint();
  expect(count('window_ready')).toBe(1);
});

test('the window shows only once the theme is applied: no dark page before a light one', async () => {
  const stored = held<Settings>();
  settingsAnswer = () => stored.promise;
  lockAnswer = () => lockState({ reason: 'startup' });
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
  await idle();
  expect(count('window_ready')).toBe(0);
  await act(async () => stored.resolve(settings({ theme: 'light' })));
  await waitFor(() => expect(count('window_ready')).toBe(1));
  expect(themeAtReveal).toBe('light');
});

test('settings that cannot be read leave the default theme, and the window shows', async () => {
  settingsAnswer = () => refusal('settings.json is unreadable');
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
  await paint();
  expect(count('window_ready')).toBe(1);
});

test('a quit waiting for operations shows that it is stopping them, not a dead page', async () => {
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
  await act(async () => {
    await emit(QUITTING, { running: 2, waitMs: 15_000 } satisfies Quitting);
  });
  expect(await screen.findByRole('heading', { name: 'Stopping 2 operations…' })).toBeDefined();
  expect(
    screen.getByText('AppRafter quits once they have stopped, within 15 seconds.'),
  ).toBeDefined();
  expect(screen.queryByRole('heading', { name: 'Open a cluster' })).toBeNull();
  expect(document.querySelector('header.titlebar')).not.toBeNull();
});

test('over the lock screen too, and in the singular for one', async () => {
  lockAnswer = () => lockState({ reason: 'startup' });
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
  await act(async () => {
    await emit(QUITTING, { running: 1, waitMs: 15_000 } satisfies Quitting);
  });
  expect(await screen.findByRole('heading', { name: 'Stopping 1 operation…' })).toBeDefined();
  expect(screen.getByText('AppRafter quits once it has stopped, within 15 seconds.')).toBeDefined();
  expect(screen.queryByRole('heading', { name: 'AppRafter is locked' })).toBeNull();
});

test('when app_info fails, the error shows under a title bar, and the window shows', async () => {
  appInfoAnswer = () => refusal('the shell has not started yet');
  settingsAnswer = () => settings({ theme: 'light' });
  render(<App />);
  expect((await screen.findByRole('alert')).textContent).toContain('the shell has not started yet');
  // Without decorations (Windows) the bar is what moves the window and closes it.
  expect(document.querySelector('header.titlebar')).not.toBeNull();
  await paint();
  expect(count('window_ready')).toBe(1);
  // The theme applies above the platform gate, so the error has it too.
  expect(themeAtReveal).toBe('light');
});

const unavailable = (reason: string) =>
  Promise.reject({
    code: DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
    message: `device-owner authentication is unavailable here (${reason})`,
    help: null,
    causes: [],
    fields: { reason },
  });

test('Linux without a polkit agent: the field comes after the refusal, and no lock keeps it', async () => {
  // As Rust: polkit is the way until it finds no agent; the PAM route and its field from then
  // on; and every lock forgets that again.
  let agentMissing = false;
  let state = lockState({ reason: 'startup', seq: 0 });
  appInfoAnswer = () =>
    appInfo({ auth: authInfo({ method: 'polkit', passwordField: agentMissing }) });
  lockAnswer = () => state;
  mockIPC(
    (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd === 'unlock') {
        agentMissing = true;
        return unavailable('no_agent');
      }
      if (cmd === 'unlock_with_password') {
        if (!agentMissing) return unavailable('use_system_prompt');
        state = lockState({ locked: false, seq: state.seq + 1 });
        return state;
      }
      if (cmd === 'app_info') return appInfoAnswer();
      if (cmd === 'lock_status') return lockAnswer();
      if (cmd === 'settings_get') return settingsAnswer();
      if (cmd === 'op_list') return [];
      return null;
    },
    { shouldMockEvents: true },
  );
  render(<App />);
  const user = userEvent.setup();
  await screen.findByRole('heading', { name: 'AppRafter is locked' });
  expect(screen.queryByLabelText('System password')).toBeNull();

  await user.click(screen.getByRole('button', { name: 'Unlock' }));
  await user.type(await screen.findByLabelText('System password'), 'hunter2{Enter}');
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
  expect(calls.find((c) => c.cmd === 'unlock_with_password')?.args).toEqual({
    password: 'hunter2',
  });

  // An idle lock: Rust forgot the missing agent, so the field would answer use_system_prompt.
  agentMissing = false;
  state = lockState({ reason: 'idle', seq: state.seq + 1 });
  await act(async () => {
    await emit(LOCK_CHANGED, state);
  });
  expect(await screen.findByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
  expect(screen.queryByLabelText('System password')).toBeNull();
  await idle();
  expect(screen.queryByLabelText('System password')).toBeNull();
  expect(screen.getByRole('button', { name: 'Unlock' }).textContent).toBe('Unlock');
  expect(count('app_info')).toBe(3);
});
