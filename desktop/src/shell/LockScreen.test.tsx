// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { AppInfo } from '../ipc/generated/AppInfo';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { LockState } from '../ipc/generated/LockState';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo, authInfo, lockState } from '../test/fixtures';
import { LockScreen } from './LockScreen';

let calls: string[];
let unlockAnswer: () => unknown;

beforeEach(() => {
  calls = [];
  unlockAnswer = () => lockState({ locked: false });
  mockIPC((cmd) => {
    calls.push(cmd);
    return cmd === 'unlock' ? unlockAnswer() : null;
  });
});

afterEach(() => {
  cleanup();
  clearMocks();
});

function lockScreen(state: LockState = lockState(), info: AppInfo = appInfo()) {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={info}>
        <LockScreen state={state} />
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}

describe('LockScreen', () => {
  test('says the app is locked', () => {
    lockScreen();
    expect(screen.getByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
  });

  test.each([
    [
      lockState({ reason: 'startup', autoLockMinutes: 10 }),
      'Locked when the app started. Auto-lock after 10 min idle.',
    ],
    [lockState({ reason: 'startup', autoLockMinutes: null }), 'Locked when the app started.'],
    [lockState({ reason: 'idle', autoLockMinutes: 30 }), 'Locked after 30 minutes of inactivity.'],
    [lockState({ reason: 'os_session' }), 'Locked when the computer locked or slept.'],
    [lockState({ reason: 'manual' }), 'Locked.'],
  ])('the reason line: %o', (state, line) => {
    lockScreen(state);
    expect(screen.getByText(line)).toBeDefined();
  });

  test('the account card: initials, the account, the OS account and host', () => {
    lockScreen(lockState(), appInfo({ os: 'windows', account: 'alex.morgan', host: 'DESKTOP-7' }));
    expect(screen.getByText('AM')).toBeDefined();
    expect(screen.getByText('alex.morgan')).toBeDefined();
    expect(screen.getByText('Windows account · DESKTOP-7')).toBeDefined();
  });

  test('one Unlock button asks Rust, which asks the OS', async () => {
    const user = lockScreen();
    await user.click(screen.getByRole('button', { name: 'Unlock' }));
    expect(calls).toEqual(['unlock']);
  });

  test.each([DESKTOP_ERROR_CODES.AUTH_CANCELLED, DESKTOP_ERROR_CODES.AUTH_FAILED])(
    'a refused unlock (%s) shows why, and Unlock can be pressed again',
    async (code) => {
      unlockAnswer = () =>
        Promise.reject({
          code,
          message: 'The owner was not verified.',
          help: null,
          causes: [],
          fields: {},
        });
      const user = lockScreen();
      await user.click(screen.getByRole('button', { name: 'Unlock' }));
      expect((await screen.findByRole('alert')).textContent).toBe('The owner was not verified.');
      expect((screen.getByRole('button', { name: 'Unlock' }) as HTMLButtonElement).disabled).toBe(
        false,
      );
    },
  );

  test('no password field: unlock_with_password does not exist yet', () => {
    lockScreen(lockState(), appInfo({ auth: authInfo({ method: 'pam', passwordField: true }) }));
    expect(screen.queryByLabelText(/password/i)).toBeNull();
  });

  test('the footer follows where credentials are kept', () => {
    lockScreen();
    expect(
      screen.getByText('Clusters keep running. Credentials are stored in files on this computer.'),
    ).toBeDefined();
    cleanup();
    lockScreen(lockState(), appInfo({ secretBackend: 'keyring' }));
    expect(
      screen.getByText('Clusters keep running. Credentials stay in the system keychain.'),
    ).toBeDefined();
  });

  test('a test build says so', () => {
    lockScreen();
    expect(screen.queryByText('TEST BUILD')).toBeNull();
    cleanup();
    lockScreen(lockState(), appInfo({ testBuild: true }));
    expect(screen.getByText('TEST BUILD')).toBeDefined();
  });
});
