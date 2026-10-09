// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, jest, test } from 'bun:test';
import { type QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { AppInfo } from '../ipc/generated/AppInfo';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { LockState } from '../ipc/generated/LockState';
import type { JsonValue } from '../ipc/generated/serde_json/JsonValue';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo, authInfo, lockState } from '../test/fixtures';
import { LockScreen } from './LockScreen';

let calls: { cmd: string; args: unknown }[];
let unlockAnswer: () => unknown;
let passwordAnswer: (password: string) => unknown;

beforeEach(() => {
  calls = [];
  unlockAnswer = () => lockState({ locked: false });
  passwordAnswer = () => lockState({ locked: false });
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    if (cmd === 'unlock') return unlockAnswer();
    if (cmd === 'unlock_with_password') {
      return passwordAnswer((args as { password: string }).password);
    }
    return null;
  });
});

afterEach(() => {
  cleanup();
  clearMocks();
});

const commands = () => calls.map((c) => c.cmd);

/** Set while a test runs on fake timers (onFakeTime). */
let fakeTime = false;

function lockScreen(
  state: LockState = lockState(),
  info: AppInfo = appInfo(),
  { client = createQueryClient() }: { client?: QueryClient } = {},
) {
  render(
    <QueryClientProvider client={client}>
      <PlatformContext value={info}>
        <LockScreen state={state} />
      </PlatformContext>
    </QueryClientProvider>,
  );
  // Under fake timers (onFakeTime) user-event must not pause between keys: nothing would end
  // the pause.
  return userEvent.setup(fakeTime ? { delay: null } : {});
}

/** Let a refused IPC call reach the page: its promise chain settles in microtasks. */
const settled = () =>
  act(async () => {
    for (let i = 0; i < 20; i += 1) await Promise.resolve();
  });

/** Run `body` with the clock faked (setTimeout and performance.now alike). */
async function onFakeTime(body: () => Promise<void>) {
  jest.useFakeTimers();
  fakeTime = true;
  try {
    await body();
  } finally {
    fakeTime = false;
    jest.useRealTimers();
  }
}

const alertLines = () =>
  [...screen.getByRole('alert').children].map((line) => line.textContent ?? '');

/** Linux where polkit cannot prompt: the PAM route, and the app's own field. */
const PAM = appInfo({ auth: authInfo({ method: 'pam', passwordField: true }) });

const refusal = (code: string, fields: Record<string, JsonValue> = {}) =>
  Promise.reject({ code, message: `Rust says ${code}`, help: null, causes: [], fields });

const passwordInput = () => screen.getByLabelText('System password') as HTMLInputElement;
const unlockButton = () => screen.getByRole('button', { name: 'Unlock' }) as HTMLButtonElement;

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
    await user.click(unlockButton());
    expect(commands()).toEqual(['unlock']);
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
      await user.click(unlockButton());
      expect((await screen.findByRole('alert')).textContent).toBe('The owner was not verified.');
      expect(unlockButton().disabled).toBe(false);
    },
  );

  test.each([
    [DESKTOP_ERROR_CODES.AUTH_BUSY, {}, 'A check is already open. Finish it, then try again.'],
    [
      DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
      { reason: 'no_agent' },
      'The system could not show its password prompt.',
    ],
  ])('a refused unlock (%s) says it plainly', async (code, fields, line) => {
    unlockAnswer = () => refusal(code, fields);
    const user = lockScreen();
    await user.click(unlockButton());
    expect((await screen.findByRole('alert')).textContent).toBe(line);
  });

  test("the OS's dialog behind Rust's back-off: Unlock waits out the countdown", () =>
    onFakeTime(async () => {
      unlockAnswer = () =>
        refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true, retryInMs: 1_200 });
      const user = lockScreen(lockState(), appInfo({ os: 'windows' }));
      await user.click(unlockButton());
      await settled();
      expect(alertLines()).toEqual(['Too many failed attempts. Try again in 2 s.']);
      expect(unlockButton().disabled).toBe(true);
      act(() => jest.advanceTimersByTime(1_200));
      expect(screen.queryByRole('alert')).toBeNull();
      expect(unlockButton().disabled).toBe(false);
    }));

  test('no password field unless app_info says the OS cannot prompt', () => {
    lockScreen();
    expect(screen.queryByLabelText(/password/i)).toBeNull();
    expect(unlockButton().textContent).toBe('Unlock');
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

describe('LockScreen where the OS cannot prompt: the password field', () => {
  test('the field instead of the Unlock button, in reach at once; Enter sends the password', async () => {
    const user = lockScreen(lockState(), PAM);
    expect(passwordInput().getAttribute('placeholder')).toBe('Password for alex');
    expect(document.activeElement).toBe(passwordInput());
    // The arrow is the only Unlock: the plain button would ask for a prompt that cannot show.
    expect(screen.getAllByRole('button', { name: 'Unlock' })).toHaveLength(1);
    expect(unlockButton().textContent).toBe('');
    await user.keyboard('hunter2{Enter}');
    expect(calls).toEqual([{ cmd: 'unlock_with_password', args: { password: 'hunter2' } }]);
  });

  test('the arrow sends it too; with the field empty nothing is sent', async () => {
    const user = lockScreen(lockState(), PAM);
    expect(unlockButton().disabled).toBe(true);
    await user.keyboard('{Enter}');
    expect(calls).toEqual([]);
    await user.type(passwordInput(), 'hunter2');
    await user.click(unlockButton());
    expect(commands()).toEqual(['unlock_with_password']);
  });

  test("a wrong password: the OS's words, the field emptied and in reach again", async () => {
    passwordAnswer = () =>
      refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, {
        exhausted: false,
        messages: ['Authentication failure'],
      });
    const user = lockScreen(lockState(), PAM);
    await user.keyboard('guess{Enter}');
    expect((await screen.findByRole('alert')).textContent).toBe('Authentication failure');
    expect(passwordInput().value).toBe('');
    expect(passwordInput().readOnly).toBe(false);
    expect(document.activeElement).toBe(passwordInput());
    // The next attempt starts clean.
    passwordAnswer = () => new Promise(() => undefined);
    await user.keyboard('again{Enter}');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  test('a refusal marks the field until the owner types again, and sits under it', async () => {
    passwordAnswer = () => refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: false });
    const user = lockScreen(lockState(), PAM);
    expect(passwordInput().getAttribute('aria-invalid')).toBeNull();
    await user.keyboard('guess{Enter}');
    const alert = await screen.findByRole('alert');
    expect(passwordInput().getAttribute('aria-invalid')).toBe('true');
    // Under the field, in its block, as the design has it.
    expect(alert.closest('.lock-password')).not.toBeNull();
    await user.keyboard('h');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(passwordInput().getAttribute('aria-invalid')).toBeNull();
  });

  test('…and when the OS said nothing, that the password is not right', async () => {
    passwordAnswer = () => refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: false });
    const user = lockScreen(lockState(), PAM);
    await user.keyboard('guess{Enter}');
    expect((await screen.findByRole('alert')).textContent).toBe('That password is not right.');
  });

  test('while the check runs the field cannot change and nothing is sent again', async () => {
    let answer!: (state: LockState) => void;
    passwordAnswer = () => new Promise((resolve) => (answer = resolve));
    const user = lockScreen(lockState(), PAM);
    await user.keyboard('hunter2{Enter}');
    expect(passwordInput().readOnly).toBe(true);
    expect(unlockButton().disabled).toBe(true);
    await user.keyboard('{Enter}');
    expect(commands()).toEqual(['unlock_with_password']);
    await act(async () => answer(lockState({ locked: false })));
    expect(passwordInput().value).toBe('');
  });

  test("too many failed attempts: a live countdown from Rust's retryInMs, then the field", () =>
    onFakeTime(async () => {
      passwordAnswer = () =>
        refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, {
          exhausted: true,
          retryInMs: 2_500,
          messages: ['Authentication failure'],
        });
      const user = lockScreen(lockState(), PAM);
      await user.keyboard('guess{Enter}');
      await settled();
      expect(alertLines()).toEqual([
        'Authentication failure',
        'Too many failed attempts. Try again in 3 s.',
      ]);
      expect(passwordInput().disabled).toBe(true);
      expect(unlockButton().disabled).toBe(true);
      act(() => jest.advanceTimersByTime(500));
      expect(alertLines()[1]).toBe('Too many failed attempts. Try again in 2 s.');
      act(() => jest.advanceTimersByTime(1_000));
      expect(alertLines()[1]).toBe('Too many failed attempts. Try again in 1 s.');
      expect(passwordInput().disabled).toBe(true);
      act(() => jest.advanceTimersByTime(1_000));
      expect(passwordInput().disabled).toBe(false);
      expect(document.activeElement).toBe(passwordInput());
      expect(alertLines()).toEqual(['Authentication failure']);
      passwordAnswer = () => lockState({ locked: false });
      await user.keyboard('hunter2{Enter}');
      expect(commands()).toEqual(['unlock_with_password', 'unlock_with_password']);
    }));

  test('turned away by the back-off itself: only the countdown, nothing about the password', () =>
    onFakeTime(async () => {
      passwordAnswer = () =>
        refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true, retryInMs: 30_000 });
      const user = lockScreen(lockState(), PAM);
      await user.keyboard('hunter2{Enter}');
      await settled();
      expect(alertLines()).toEqual(['Too many failed attempts. Try again in 30 s.']);
      act(() => jest.advanceTimersByTime(30_000));
      expect(screen.queryByRole('alert')).toBeNull();
      expect(passwordInput().disabled).toBe(false);
    }));

  test('too many attempts with no end said: try again later, and the field stays', async () => {
    passwordAnswer = () =>
      refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true, messages: [] });
    const user = lockScreen(lockState(), PAM);
    await user.keyboard('guess{Enter}');
    expect((await screen.findByRole('alert')).textContent).toBe(
      'Too many failed attempts. Try again later.',
    );
    expect(passwordInput().disabled).toBe(false);
  });

  test('busy: a check is already open', async () => {
    passwordAnswer = () => refusal(DESKTOP_ERROR_CODES.AUTH_BUSY);
    const user = lockScreen(lockState(), PAM);
    await user.keyboard('hunter2{Enter}');
    expect((await screen.findByRole('alert')).textContent).toBe(
      'A check is already open. Finish it, then try again.',
    );
  });

  test('where the OS prompts after all, it says so', async () => {
    passwordAnswer = () =>
      refusal(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'not_permitted_here' });
    const user = lockScreen(lockState(), PAM);
    await user.keyboard('hunter2{Enter}');
    expect((await screen.findByRole('alert')).textContent).toBe(
      'The system asks for your password itself now. Try again.',
    );
  });

  test('the password is kept nowhere but the field: no cache entry, no log line', async () => {
    passwordAnswer = () =>
      refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: false, messages: ['nope'] });
    const client = createQueryClient();
    const logged: unknown[] = [];
    const saved = { error: console.error, warn: console.warn, log: console.log };
    console.error = (...args: unknown[]) => logged.push(args);
    console.warn = (...args: unknown[]) => logged.push(args);
    console.log = (...args: unknown[]) => logged.push(args);
    try {
      const user = lockScreen(lockState(), PAM, { client });
      await user.keyboard('correct-horse{Enter}');
      await screen.findByRole('alert');
    } finally {
      Object.assign(console, saved);
    }
    const kept = JSON.stringify([
      client
        .getQueryCache()
        .getAll()
        .map((query) => query.state),
      client
        .getMutationCache()
        .getAll()
        .map((mutation) => mutation.state),
    ]);
    expect(kept).not.toContain('correct-horse');
    expect(JSON.stringify(logged.map(String))).not.toContain('correct-horse');
    expect(passwordInput().value).toBe('');
  });

  test('an unlock refused with no_agent can bring the field, its line staying with it', async () => {
    unlockAnswer = () => refusal(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'no_agent' });
    const client = createQueryClient();
    const view = (info: AppInfo) => (
      <QueryClientProvider client={client}>
        <PlatformContext value={info}>
          <LockScreen state={lockState()} />
        </PlatformContext>
      </QueryClientProvider>
    );
    const { rerender } = render(view(appInfo()));
    const user = userEvent.setup();
    await user.click(unlockButton());
    await screen.findByRole('alert');
    // PlatformGate read app_info again, and it says the PAM route now.
    rerender(view(PAM));
    expect(document.activeElement).toBe(passwordInput());
    expect(screen.getByRole('alert').textContent).toBe(
      'The system could not show its password prompt.',
    );
  });
});
