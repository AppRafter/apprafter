// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { IpcError } from '../ipc/api';
import type { Theme } from '../ipc/generated/Theme';
import { settleIpc } from '../test/settle';
import { applyTheme, followTheme, resolveTheme, watchSystemTheme } from './theme';

const QUERY = '(prefers-color-scheme: dark)';

type Listener = (event: { matches: boolean }) => void;

/** A stand-in for the OS appearance: `matchMedia(QUERY)` whose answer the test flips. */
function stubSystem(prefersDark: boolean) {
  const listeners = new Set<Listener>();
  const list = {
    media: QUERY,
    matches: prefersDark,
    addEventListener: (type: string, listener: Listener) => {
      expect(type).toBe('change');
      listeners.add(listener);
    },
    removeEventListener: (type: string, listener: Listener) => {
      expect(type).toBe('change');
      listeners.delete(listener);
    },
  };
  window.matchMedia = ((query: string) => {
    expect(query).toBe(QUERY);
    return list;
  }) as unknown as typeof window.matchMedia;
  return {
    listeners,
    flip(dark: boolean) {
      list.matches = dark;
      for (const listener of [...listeners]) listener({ matches: dark });
    },
  };
}

const originalMatchMedia = window.matchMedia;
let calls: { cmd: string; args: unknown }[];

beforeEach(() => {
  calls = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return null;
  });
});

afterEach(async () => {
  await settleIpc();
  clearMocks();
  window.matchMedia = originalMatchMedia;
  delete document.documentElement.dataset.theme;
});

/** Lets the window API's promise chain reach the mocked invoke. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** What the page asked Rust to give the native window, in order. */
const themeApplyCalls = () => calls.filter((c) => c.cmd === 'theme_apply').map((c) => c.args);

/** The page never sets the window's theme itself: the window plugin's `null` forces light on Linux. */
const expectNoWindowSetTheme = () =>
  expect(calls.filter((c) => c.cmd.startsWith('plugin:window|'))).toEqual([]);

describe('resolveTheme', () => {
  const cases: [Theme, boolean, 'light' | 'dark'][] = [
    ['light', true, 'light'],
    ['light', false, 'light'],
    ['dark', true, 'dark'],
    ['dark', false, 'dark'],
    ['system', true, 'dark'],
    ['system', false, 'light'],
  ];
  test.each(cases)('%s with a dark system=%p is %s', (setting, prefersDark, resolved) => {
    expect(resolveTheme(setting, prefersDark)).toBe(resolved);
  });
});

describe('applyTheme', () => {
  test('an explicit theme goes to the page and, through Rust, to the native window', async () => {
    await applyTheme('light', true);
    expect(document.documentElement.dataset.theme).toBe('light');
    await applyTheme('dark', false);
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(themeApplyCalls()).toEqual([{ theme: 'light' }, { theme: 'dark' }]);
    expectNoWindowSetTheme();
  });

  test('under system the page follows the OS, and Rust is told system, never a null theme', async () => {
    // Rust leaves the window to the OS on macOS and Windows, and on Linux resolves it from the
    // desktop: the window plugin's setTheme(null) would force light there (tao).
    await applyTheme('system', true);
    expect(document.documentElement.dataset.theme).toBe('dark');
    await applyTheme('system', false);
    expect(document.documentElement.dataset.theme).toBe('light');
    expect(themeApplyCalls()).toEqual([{ theme: 'system' }, { theme: 'system' }]);
    expectNoWindowSetTheme();
  });

  test('the page theme is set even when the native call is refused', async () => {
    clearMocks();
    mockIPC(() => Promise.reject('theme_apply not allowed'));
    const applied = applyTheme('light', false);
    await expect(applied).rejects.toBeInstanceOf(IpcError);
    await expect(applied).rejects.toThrow('theme_apply not allowed');
    expect(document.documentElement.dataset.theme).toBe('light');
  });
});

describe('watchSystemTheme', () => {
  test('reports each change of the OS appearance until unsubscribed', () => {
    const system = stubSystem(false);
    const seen: boolean[] = [];
    const stop = watchSystemTheme((dark) => seen.push(dark));
    system.flip(true);
    system.flip(false);
    stop();
    system.flip(true);
    expect(seen).toEqual([true, false]);
    expect(system.listeners.size).toBe(0);
  });
});

describe('followTheme', () => {
  test('under system the page follows each OS change; Rust is told system once', async () => {
    const system = stubSystem(true);
    const stop = followTheme('system');
    await settle();
    expect(document.documentElement.dataset.theme).toBe('dark');

    system.flip(false);
    await settle();
    expect(document.documentElement.dataset.theme).toBe('light');
    system.flip(true);
    await settle();
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(themeApplyCalls()).toEqual([{ theme: 'system' }]);
    expectNoWindowSetTheme();

    stop();
    expect(system.listeners.size).toBe(0);
  });

  test('a refused native call is reported, and the page keeps its theme', async () => {
    stubSystem(false);
    clearMocks();
    mockIPC(() => Promise.reject('not allowed'));
    const reported = spyOn(console, 'error').mockImplementation(() => {});
    try {
      const stop = followTheme('dark');
      await settle();
      expect(document.documentElement.dataset.theme).toBe('dark');
      expect(reported).toHaveBeenCalledTimes(1);
      const [message, error] = reported.mock.calls[0] ?? [];
      expect(message).toBe('the window theme was not applied:');
      expect(error).toBeInstanceOf(IpcError);
      expect((error as IpcError).message).toBe('not allowed');
      stop();
    } finally {
      reported.mockRestore();
    }
  });

  test.each(['light', 'dark'] as const)(
    'under %s an OS change re-resolves nothing',
    async (theme) => {
      const system = stubSystem(theme === 'light');
      const stop = followTheme(theme);
      await settle();
      system.flip(theme !== 'light');
      await settle();
      expect(document.documentElement.dataset.theme).toBe(theme);
      expect(themeApplyCalls()).toEqual([{ theme }]);
      expectNoWindowSetTheme();
      expect(system.listeners.size).toBe(0);
      stop();
    },
  );
});
