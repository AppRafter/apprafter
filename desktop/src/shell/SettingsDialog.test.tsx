// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../components/Toast';
import type { AppInfo } from '../ipc/generated/AppInfo';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { Settings } from '../ipc/generated/Settings';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo, authInfo, settings } from '../test/fixtures';
import { SettingsDialog } from './SettingsDialog';

interface HeldSave {
  readonly request: Settings;
  readonly resolve: (inUse: Settings) => void;
  readonly reject: (error: unknown) => void;
}

let calls: { cmd: string; args: Record<string, unknown> }[];
let stored: Settings;
let refuse: string | null;
/** Saves wait in `held` until answerHeld(); off, each is answered at once. */
let holding: boolean;
let held: HeldSave[];

const refusal = (message: string) => ({
  code: DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
  message,
  help: null,
  causes: [],
  fields: {},
});

/** Rust's settings_set, as far as these tests need it: the lock cannot be switched on. */
function answer(save: HeldSave) {
  if (save.request.lockEnabled && !stored.lockEnabled) {
    save.reject(refusal('No system authentication.'));
    return;
  }
  stored = save.request;
  save.resolve(stored);
}

/**
 * Answers the held saves, the newest first, until none is left: what two saves in flight at
 * once would meet if the second reached Rust first.
 */
async function answerHeld() {
  for (let quiet = 0; quiet < 3; ) {
    await act(() => new Promise((resolve) => setTimeout(resolve, 10)));
    const batch = held.splice(0).reverse();
    if (batch.length === 0) {
      quiet += 1;
      continue;
    }
    quiet = 0;
    await act(async () => {
      for (const save of batch) answer(save);
    });
  }
}

beforeEach(() => {
  calls = [];
  stored = settings();
  refuse = null;
  holding = false;
  held = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: args as Record<string, unknown> });
    if (cmd === 'settings_get') return stored;
    if (cmd === 'settings_set' && holding) {
      const request = (args as { settings: Settings }).settings;
      return new Promise((resolve, reject) => held.push({ request, resolve, reject }));
    }
    if (cmd === 'settings_set') {
      if (refuse !== null) {
        return Promise.reject({
          code: refuse,
          message: 'No system authentication.',
          help: null,
          causes: [],
          fields: {},
        });
      }
      stored = (args as { settings: Settings }).settings;
      return stored;
    }
    return null;
  });
});

afterEach(() => {
  cleanup();
  clearMocks();
});

async function open(info: AppInfo = appInfo()) {
  const onClose = mock();
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={info}>
        <ToastProvider>
          <SettingsDialog onClose={onClose} />
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  await screen.findByRole('radiogroup', { name: 'Theme' });
  return { user: userEvent.setup(), onClose };
}

const saved = () =>
  calls.filter((c) => c.cmd === 'settings_set').map((c) => c.args.settings as Settings);
const lastSaved = () => saved().at(-1);

describe('SettingsDialog', () => {
  test('each control saves the setting it shows', async () => {
    const { user } = await open(appInfo({ auth: authInfo({ biometricsChoice: true }) }));
    await user.click(screen.getByRole('radio', { name: 'Light' }));
    expect(lastSaved()?.theme).toBe('light');
    await user.click(screen.getByRole('switch', { name: 'Lock when the app starts' }));
    expect(lastSaved()?.lockOnStart).toBe(false);
    await user.click(screen.getByRole('switch', { name: 'Prefer biometrics' }));
    expect(lastSaved()?.hello).toBe(false);
    await user.click(screen.getByRole('radio', { name: '30 min' }));
    expect(lastSaved()?.autoLock).toBe('30');
    await user.click(screen.getByRole('switch', { name: 'Require unlock' }));
    expect(lastSaved()?.lockEnabled).toBe(false);
    // Each save carries the others as they are now.
    expect(lastSaved()).toEqual(
      settings({
        theme: 'light',
        lockOnStart: false,
        hello: false,
        autoLock: '30',
        lockEnabled: false,
      }),
    );
  });

  test('with the lock off, what depends on it is disabled, not just dimmed', async () => {
    stored = settings({ lockEnabled: false });
    await open();
    expect(
      (screen.getByRole('switch', { name: 'Lock when the app starts' }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    for (const radio of within(
      screen.getByRole('radiogroup', { name: 'Auto-lock after inactivity' }),
    ).getAllByRole('radio')) {
      expect((radio as HTMLButtonElement).disabled).toBe(true);
    }
    expect((screen.getByRole('button', { name: 'Lock' }) as HTMLButtonElement).disabled).toBe(true);
    expect(
      (screen.getByRole('switch', { name: 'Require unlock' }) as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  test('rows with nothing behind them yet stay hidden', async () => {
    await open();
    expect(screen.queryByRole('switch', { name: 'Prefer biometrics' })).toBeNull();
    for (const name of [/sleep/i, /Refresh/, /Pause/, /Notify/, /tray/i]) {
      expect(screen.queryByText(name)).toBeNull();
    }
  });

  test('the banner shows exactly when no system authentication is available', async () => {
    await open();
    const banner =
      'This computer offers no system authentication AppRafter can use, so the app lock is off.';
    expect(screen.queryByText(banner)).toBeNull();
    cleanup();
    await open(
      appInfo({ auth: authInfo({ available: false, method: null, unavailable: 'no_backend' }) }),
    );
    expect(screen.getByText(banner)).toBeDefined();
  });

  test('a refused save puts the control back and says why', async () => {
    stored = settings({ lockEnabled: false });
    refuse = DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE;
    const { user } = await open();
    const master = screen.getByRole('switch', { name: 'Require unlock' });
    await user.click(master);
    await waitFor(() => expect(master.getAttribute('aria-checked')).toBe('false'));
    expect(screen.getByRole('status').textContent).toContain('No system authentication.');
  });

  test('a refused save, then another change: both end on what Rust holds, plus the change', async () => {
    stored = settings({ lockEnabled: false, theme: 'dark' });
    holding = true;
    const { user } = await open();
    await user.click(screen.getByRole('switch', { name: 'Require unlock' }));
    await user.click(screen.getByRole('radio', { name: 'Light' }));
    await answerHeld();
    expect(stored).toEqual(settings({ lockEnabled: false, theme: 'light' }));
    expect(
      screen.getByRole('switch', { name: 'Require unlock' }).getAttribute('aria-checked'),
    ).toBe('false');
    expect(screen.getByRole('radio', { name: 'Light' }).getAttribute('aria-checked')).toBe('true');
    expect(screen.getByRole('status').textContent).toContain('No system authentication.');
    // The refusal is not undone from a snapshot: the settings are read again.
    expect(calls.filter((c) => c.cmd === 'settings_get').length).toBe(2);
  });

  test('a save waiting behind another never carries a later save’s change', async () => {
    stored = settings({ lockEnabled: false, theme: 'dark' });
    holding = true;
    const { user } = await open();
    await user.click(screen.getByRole('radio', { name: 'Light' }));
    await user.click(screen.getByRole('radio', { name: 'System' }));
    await user.click(screen.getByRole('switch', { name: 'Require unlock' }));
    await answerHeld();
    // The lock was refused; the theme the owner picked before it was not dragged down with it.
    expect(stored).toEqual(settings({ lockEnabled: false, theme: 'system' }));
    expect(saved().map((s) => [s.theme, s.lockEnabled])).toEqual([
      ['light', false],
      ['system', false],
      ['system', true],
    ]);
  });

  test('two quick choices, whatever order Rust would answer them in: the last one wins', async () => {
    stored = settings({ autoLock: '10' });
    holding = true;
    const { user } = await open();
    await user.click(screen.getByRole('radio', { name: '5 min' }));
    await user.click(screen.getByRole('radio', { name: '30 min' }));
    // Optimistic meanwhile: the dialog shows the last choice at once.
    expect(screen.getByRole('radio', { name: '30 min' }).getAttribute('aria-checked')).toBe('true');
    await answerHeld();
    expect(stored.autoLock).toBe('30');
    expect(screen.getByRole('radio', { name: '30 min' }).getAttribute('aria-checked')).toBe('true');
    // One at a time, each from the settings Rust confirmed plus its own change.
    expect(saved().map((s) => s.autoLock)).toEqual(['5', '30']);
  });

  test('copy follows the OS and its authentication', async () => {
    await open(appInfo({ os: 'macos', auth: authInfo({ method: 'mac_local_authentication' }) }));
    expect(screen.getByText('System follows the macOS appearance.')).toBeDefined();
    expect(
      screen.getByText('Ask for Touch ID or your Mac password before showing any cluster.'),
    ).toBeDefined();
    expect(screen.getByText('Also ⌘L.')).toBeDefined();
  });

  test('Lock now locks; About shows the versions and opens the links', async () => {
    const { user } = await open();
    await user.click(screen.getByRole('button', { name: 'Lock' }));
    expect(calls.map((c) => c.cmd)).toContain('lock_now');
    expect(screen.getByText('AppRafter Desktop 0.1.0 · core 0.2.80')).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Docs' }));
    expect(calls.find((c) => c.cmd === 'plugin:opener|open_url')?.args.url).toBe(
      'https://docs.apprafter.dev',
    );
  });

  test('a notice about the settings file is shown when Rust has one', async () => {
    await open(appInfo({ settingsNotice: 'settings.json is newer than this app; using defaults' }));
    expect(screen.getByText('settings.json is newer than this app; using defaults')).toBeDefined();
  });
});
