// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import type { Theme } from '../ipc/generated/Theme';
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
  // Before anything touches @tauri-apps/api/window: getCurrentWindow() reads this metadata.
  mockWindows('main');
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return null;
  });
});

afterEach(() => {
  clearMocks();
  window.matchMedia = originalMatchMedia;
  delete document.documentElement.dataset.theme;
});

/** Lets the window API's promise chain reach the mocked invoke. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

const setThemeCalls = () =>
  calls.filter((c) => c.cmd === 'plugin:window|set_theme').map((c) => c.args);

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
  test('an explicit theme goes to the page and to the native window alike', async () => {
    await applyTheme('light', true);
    expect(document.documentElement.dataset.theme).toBe('light');
    await applyTheme('dark', false);
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(setThemeCalls()).toEqual([
      { label: 'main', value: 'light' },
      { label: 'main', value: 'dark' },
    ]);
  });

  test('under system the page follows the OS, and the native window is left to the OS', async () => {
    // A forced window theme would force the webview's prefers-color-scheme too (macOS sets
    // NSApp.appearance app-wide), and the OS change would never reach the page again.
    await applyTheme('system', true);
    expect(document.documentElement.dataset.theme).toBe('dark');
    await applyTheme('system', false);
    expect(document.documentElement.dataset.theme).toBe('light');
    expect(setThemeCalls()).toEqual([
      { label: 'main', value: null },
      { label: 'main', value: null },
    ]);
  });

  test('the page theme is set even when the native call is refused', async () => {
    clearMocks();
    mockWindows('main');
    mockIPC(() => Promise.reject('core:window:allow-set-theme not allowed'));
    await expect(applyTheme('light', false)).rejects.toBe(
      'core:window:allow-set-theme not allowed',
    );
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
  test('under system the page follows each OS change; the window is left to the OS once', async () => {
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
    expect(setThemeCalls()).toEqual([{ label: 'main', value: null }]);

    stop();
    expect(system.listeners.size).toBe(0);
  });

  test('a refused native call is reported, and the page keeps its theme', async () => {
    stubSystem(false);
    clearMocks();
    mockWindows('main');
    mockIPC(() => Promise.reject('not allowed'));
    const reported = spyOn(console, 'error').mockImplementation(() => {});
    try {
      const stop = followTheme('dark');
      await settle();
      expect(document.documentElement.dataset.theme).toBe('dark');
      expect(reported).toHaveBeenCalledWith('the window theme was not applied:', 'not allowed');
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
      expect(setThemeCalls()).toEqual([{ label: 'main', value: theme }]);
      expect(system.listeners.size).toBe(0);
      stop();
    },
  );
});
