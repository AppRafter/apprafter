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
import { PlatformGate } from './PlatformGate';
import { SettingsDialog } from './SettingsDialog';

interface HeldSave {
  readonly request: Settings;
  readonly resolve: (inUse: Settings) => void;
  readonly reject: (error: unknown) => void;
}

let calls: { cmd: string; args: Record<string, unknown> }[];
/** What app_info answers, for the tests that render the PlatformGate; `later` from the second. */
let info: AppInfo;
let later: AppInfo | null;
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
  info = appInfo();
  later = null;
  stored = settings();
  refuse = null;
  holding = false;
  held = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: args as Record<string, unknown> });
    if (cmd === 'app_info') return reads() > 1 && later !== null ? later : info;
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

/** Settings under the PlatformGate, as the app has it: app_info read by the gate. */
async function openInGate() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformGate>
        <ToastProvider>
          <SettingsDialog onClose={mock()} />
          <ToastViewport />
        </ToastProvider>
      </PlatformGate>
    </QueryClientProvider>,
  );
  await screen.findByRole('radiogroup', { name: 'Theme' });
  return userEvent.setup();
}

const reads = () => calls.filter((c) => c.cmd === 'app_info').length;

const SLEEP_ROW = 'Lock when the computer sleeps or locks';
const sleepSwitch = () => screen.getByRole('switch', { name: SLEEP_ROW }) as HTMLButtonElement;
const sleepRow = () => sleepSwitch().closest('.row') as HTMLElement;

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
    await user.click(screen.getByRole('switch', { name: SLEEP_ROW }));
    expect(lastSaved()?.lockOnSleep).toBe(false);
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
        lockOnSleep: false,
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
    expect(sleepSwitch().disabled).toBe(true);
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
    for (const name of [/Refresh/, /Pause/, /Notify/, /tray/i]) {
      expect(screen.queryByText(name)).toBeNull();
    }
  });

  test('locking with the computer: on while the computer tells both its locks and sleeps', async () => {
    await open();
    expect(sleepSwitch().disabled).toBe(false);
    expect(sleepSwitch().getAttribute('aria-checked')).toBe('true');
    expect(sleepRow().textContent).toContain(
      'When the screen locks or the computer goes to sleep.',
    );
    expect(sleepRow().dataset.disabled).toBeUndefined();
  });

  test('…on all the same when the computer tells neither, the note naming the limit', async () => {
    // Every source is still listened to — a `loginctl lock-session`, a screen saver started
    // later — so the owner must be able to switch locking with them off.
    const { user } = await open(appInfo({ sessionEvents: { lock: false, sleep: false } }));
    expect(sleepSwitch().disabled).toBe(false);
    expect(sleepRow().dataset.disabled).toBeUndefined();
    expect(sleepRow().textContent).toContain(
      'This computer does not tell AppRafter when it locks or sleeps.',
    );
    await user.click(sleepSwitch());
    expect(lastSaved()?.lockOnSleep).toBe(false);
  });

  test.each([
    [
      { lock: true, sleep: false },
      'AppRafter is told when the screen locks, not when the computer sleeps.',
    ],
    [
      { lock: false, sleep: true },
      'AppRafter is told when the computer sleeps, not when the screen locks.',
    ],
  ])(
    '…on, naming what is missing, when only one half is told (%o)',
    async (sessionEvents, note) => {
      await open(appInfo({ sessionEvents }));
      expect(sleepSwitch().disabled).toBe(false);
      expect(sleepRow().textContent).toContain(note);
    },
  );

  test('…and off with the lock, whatever the computer tells: the A1 rule', async () => {
    stored = settings({ lockEnabled: false });
    await open(appInfo({ sessionEvents: { lock: true, sleep: false } }));
    expect(sleepSwitch().disabled).toBe(true);
    cleanup();
    await open(
      appInfo({ auth: authInfo({ available: false, method: null, unavailable: 'no_backend' }) }),
    );
    expect(sleepSwitch().disabled).toBe(true);
  });

  test('a session watch that answered late: the row follows the read Settings makes', async () => {
    // The first read is the gate's, at start; the watch has said nothing yet.
    info = appInfo({ sessionEvents: { lock: false, sleep: false } });
    later = appInfo({ sessionEvents: { lock: true, sleep: true } });
    await openInGate();
    await waitFor(() =>
      expect(sleepRow().textContent).toContain(
        'When the screen locks or the computer goes to sleep.',
      ),
    );
    expect(reads()).toBe(2);
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
    const status = screen.getByRole('status');
    expect(status.textContent).toContain('No system authentication.');
    // Announced: the modal leaves the live region out of what it makes inert.
    expect(status.closest('[inert]')).toBeNull();
  });

  test('with no system authentication the lock shows as it is: off, and nothing to switch', async () => {
    stored = settings({ lockEnabled: true });
    await open(
      appInfo({ auth: authInfo({ available: false, method: null, unavailable: 'no_backend' }) }),
    );
    const master = screen.getByRole('switch', { name: 'Require unlock' }) as HTMLButtonElement;
    expect(master.getAttribute('aria-checked')).toBe('false');
    expect(master.disabled).toBe(true);
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

  test('opening Settings reads app_info again: what it says may have changed since', async () => {
    await openInGate();
    await waitFor(() => expect(reads()).toBe(2));
  });

  test('a notice about the settings file is shown when Rust has one', async () => {
    await open(appInfo({ settingsNotice: 'settings.json is newer than this app; using defaults' }));
    expect(screen.getByText('settings.json is newer than this app; using defaults')).toBeDefined();
  });
});
