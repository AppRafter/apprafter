// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { type QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, render, screen } from '@testing-library/react';
import * as api from '../ipc/api';
import type { AppInfo } from '../ipc/generated/AppInfo';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import { rereadAppInfo, usePlatform } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo, authInfo } from '../test/fixtures';
import { PlatformGate } from './PlatformGate';

let calls: string[];
/** What app_info answers next: an AppInfo, or a promise the test resolves. */
let answer: () => AppInfo | Promise<AppInfo>;
let refusal: () => unknown;

const withField = (passwordField: boolean) =>
  appInfo({ auth: authInfo({ method: 'pam', passwordField }) });

function held<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (e: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const uiError = (code: string, fields: Record<string, unknown> = {}) => ({
  code,
  message: 'refused',
  help: null,
  causes: [],
  fields,
});

beforeEach(() => {
  calls = [];
  answer = () => withField(false);
  refusal = () => Promise.reject(uiError(DESKTOP_ERROR_CODES.AUTH_FAILED));
  mockWindows('main');
  mockIPC((cmd) => {
    calls.push(cmd);
    if (cmd === 'app_info') return answer();
    if (cmd === 'unlock') return refusal();
    if (cmd === 'plugin:window|is_maximized') return false;
    return null;
  });
});

afterEach(() => clearMocks());

const settle = () => act(() => new Promise((resolve) => setTimeout(resolve, 10)));
const reads = () => calls.filter((c) => c === 'app_info').length;

function Probe() {
  const { auth, sessionEvents } = usePlatform();
  return (
    <p>
      {`field ${auth.passwordField ? 'shown' : 'hidden'}; ` +
        `session ${sessionEvents.lock ? 'lock' : '-'} ${sessionEvents.sleep ? 'sleep' : '-'}`}
    </p>
  );
}

async function gate(client: QueryClient = createQueryClient()) {
  render(
    <QueryClientProvider client={client}>
      <PlatformGate>
        <Probe />
      </PlatformGate>
    </QueryClientProvider>,
  );
  await screen.findByText(/^field/);
  return client;
}

const field = () => screen.getByText(/^field/).textContent?.split(';')[0];

describe('PlatformGate', () => {
  test('an auth refusal from any command reads app_info again', async () => {
    await gate();
    expect(field()).toBe('field hidden');
    // Linux: polkit found no agent, so the PAM route and its field are the way now.
    refusal = () =>
      Promise.reject(uiError(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'no_agent' }));
    answer = () => withField(true);
    await act(() => api.unlock().catch(() => undefined));
    await settle();
    expect(reads()).toBe(2);
    expect(field()).toBe('field shown');
  });

  test('after auth_unavailable the field is withheld until app_info answers again', async () => {
    answer = () => withField(true);
    await gate();
    expect(field()).toBe('field shown');
    const reading = held<AppInfo>();
    answer = () => reading.promise;
    refusal = () =>
      Promise.reject(
        uiError(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'not_permitted_here' }),
      );
    await act(() => api.unlock().catch(() => undefined));
    await settle();
    expect(reads()).toBe(2);
    expect(field()).toBe('field hidden');
    await act(async () => reading.resolve(withField(false)));
    expect(field()).toBe('field hidden');
  });

  test.each([DESKTOP_ERROR_CODES.AUTH_FAILED, DESKTOP_ERROR_CODES.AUTH_BUSY])(
    'after %s the field stays while app_info is read again: the owner types on',
    async (code) => {
      answer = () => withField(true);
      await gate();
      const reading = held<AppInfo>();
      answer = () => reading.promise;
      refusal = () => Promise.reject(uiError(code));
      await act(() => api.unlock().catch(() => undefined));
      await settle();
      expect(reads()).toBe(2);
      expect(field()).toBe('field shown');
      await act(async () => reading.resolve(withField(true)));
      expect(field()).toBe('field shown');
    },
  );

  test('a refusal that says nothing of authentication reads nothing', async () => {
    await gate();
    refusal = () => Promise.reject(uiError(DESKTOP_ERROR_CODES.AUTH_CANCELLED));
    await act(() => api.unlock().catch(() => undefined));
    await settle();
    expect(reads()).toBe(1);
  });

  test('a read again that fails keeps the app on what it knew, without the field', async () => {
    answer = () => withField(true);
    const client = await gate();
    answer = () => Promise.reject(uiError(DESKTOP_ERROR_CODES.INTERNAL));
    const errors: unknown[] = [];
    const original = console.error;
    console.error = (...args: unknown[]) => errors.push(args);
    try {
      await act(async () => rereadAppInfo(client, false));
      await settle();
    } finally {
      console.error = original;
    }
    // Still the app, not app_info's error screen: only the field is in doubt.
    expect(field()).toBe('field hidden');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(errors).toHaveLength(1);
  });

  test('a later answer that says more of the session reaches the app', async () => {
    answer = () => appInfo({ sessionEvents: { lock: false, sleep: false } });
    const client = await gate();
    expect(screen.getByText(/^field/).textContent).toContain('session - -');
    answer = () => appInfo({ sessionEvents: { lock: true, sleep: false } });
    await act(async () => rereadAppInfo(client, false));
    await settle();
    expect(screen.getByText(/^field/).textContent).toContain('session lock -');
  });

  test('a first read that fails is shown as it is', async () => {
    answer = () => Promise.reject(uiError(DESKTOP_ERROR_CODES.INTERNAL));
    render(
      <QueryClientProvider client={createQueryClient()}>
        <PlatformGate>
          <Probe />
        </PlatformGate>
      </QueryClientProvider>,
    );
    expect((await screen.findByRole('alert')).textContent).toContain('refused');
  });
});
