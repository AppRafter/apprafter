// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider } from '../components/Toast';
import * as api from '../ipc/api';
import { keepEndedAway, resetEndedAway } from '../ipc/away';
import type { AppInfo } from '../ipc/generated/AppInfo';
import type { OpSummary } from '../ipc/generated/OpSummary';
import type { PlanView } from '../ipc/generated/PlanView';
import type { Settings } from '../ipc/generated/Settings';
import type { TargetListReport } from '../ipc/generated/TargetListReport';
import { installMockIpc } from '../ipc/mock';
import { refreshList, resetOperations } from '../ipc/operations';
import { NO_AUTH_PROTECTED_NOTICE } from '../state/lockText';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { lockState, settings, targetSummary } from '../test/fixtures';
import { whoamiReport } from '../test/flows';
import { settleIpc } from '../test/settle';
import { PlatformGate } from './PlatformGate';
import { Shell } from './Shell';

const INFO: AppInfo = {
  os: 'windows',
  desktopVersion: '0.1.0',
  coreVersion: '0.2.80',
  secrets: {
    mode: 'files',
    slots: [],
    protected: false,
    store: '~/.config/apprafter',
    problem: null,
  },
  account: 'alex',
  host: 'workstation',
  auth: {
    available: true,
    method: 'windows_hello',
    unavailable: null,
    biometricsChoice: true,
    passwordField: false,
  },
  sessionEvents: { lock: true, sleep: true },
  testBuild: false,
  settingsNotice: null,
};

const NO_AUTH: AppInfo = {
  ...INFO,
  auth: {
    available: false,
    method: null,
    unavailable: 'no_backend',
    biometricsChoice: false,
    passwordField: false,
  },
};

const NO_AUTH_NOTICE =
  'This computer offers no system authentication AppRafter can use, so the app lock is off.';

/** target_list: prod-eu (the CLI default), staging and lab. */
const TARGETS: TargetListReport = {
  targets: [
    targetSummary(),
    targetSummary({ name: 'staging', region: 'fsn1', isCliDefault: false }),
    targetSummary({ name: 'lab', region: 'hel1', isCliDefault: false }),
  ],
  unreadable: [],
  cliDefault: { status: 'set', name: 'prod-eu' },
};

let calls: string[];
let summaries: OpSummary[];
let stored: Settings;
let appInfoNow: AppInfo;

beforeEach(() => {
  calls = [];
  summaries = [];
  stored = settings();
  appInfoNow = INFO;
  mockWindows('main');
  mockIPC(
    (cmd) => {
      calls.push(cmd);
      if (cmd === 'op_list') return summaries;
      if (cmd === 'target_list') return TARGETS;
      if (cmd === 'settings_get') return stored;
      if (cmd === 'app_info') return appInfoNow;
      if (cmd === 'lock_now') return lockState({ reason: 'manual' });
      if (cmd === 'plugin:window|is_maximized') return false;
      return null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  resetEndedAway();
  clearMocks();
});

function shell(info: AppInfo = INFO) {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={info}>
        <ToastProvider>
          <Shell />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}

/** The Shell under the platform gate, as the app has it: app_info is `appInfoNow`. */
async function gatedShell() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformGate>
        <ToastProvider>
          <Shell />
        </ToastProvider>
      </PlatformGate>
    </QueryClientProvider>,
  );
  await screen.findByRole('heading', { level: 1, name: 'Open a cluster' });
  return userEvent.setup();
}

const count = (cmd: string) => calls.filter((c) => c === cmd).length;

const tab = (name: string | RegExp) => screen.getByRole('tab', { name });
const pageTitle = () => screen.getByRole('heading', { level: 1 }).textContent;

describe('Shell', () => {
  test('the end of a plan whose screen went shows in the shell, and its operation goes', async () => {
    keepEndedAway({ opId: 41, text: 'Add target lab failed: cx22 is sold out', failed: true });
    shell();
    expect(await screen.findByText('Add target lab failed: cx22 is sold out')).toBeDefined();
    expect(calls).toContain('op_discard');
  });

  test('Settings opens from the sidebar footer and with Ctrl+,', async () => {
    const user = shell();
    await user.click(screen.getByRole('button', { name: 'Settings Ctrl+,' }));
    expect(await screen.findByRole('dialog', { name: 'Settings' })).toBeDefined();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog', { name: 'Settings' })).toBeNull();
    // By physical key: user-event's default key map has no "," (the shortcut matches `code`).
    await user.keyboard('{Control>}[Comma]{/Control}');
    expect(await screen.findByRole('dialog', { name: 'Settings' })).toBeDefined();
  });

  test('starts on the Targets view; a card opens its target in a tab on Overview', async () => {
    const user = shell();
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    expect(tab('prod-eu').getAttribute('aria-selected')).toBe('true');
    expect(pageTitle()).toBe('Overview');
    expect(screen.queryByRole('heading', { name: 'Open a cluster' })).toBeNull();
  });

  test('a target with a tab: its card switches to that tab, and opens no second one', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    await user.click(screen.getByRole('button', { name: 'Backups' }));
    await user.click(screen.getByRole('button', { name: 'Open a cluster' }));
    await user.click(screen.getByRole('button', { name: 'Switch to prod-eu' }));
    expect(tab('prod-eu').getAttribute('aria-selected')).toBe('true');
    expect(screen.getAllByRole('tab')).toHaveLength(1);
    expect(pageTitle()).toBe('Backups');
    expect(screen.queryByRole('button', { name: 'Switch to staging' })).toBeNull();
  });

  test('each tab keeps its own section across switches', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    await user.click(screen.getByRole('button', { name: 'Backups' }));
    expect(pageTitle()).toBe('Backups');
    await user.click(screen.getByRole('button', { name: 'Open a cluster' }));
    await user.click(await screen.findByRole('button', { name: 'Open staging' }));
    expect(pageTitle()).toBe('Overview');
    await user.click(tab('prod-eu'));
    expect(pageTitle()).toBe('Backups');
  });

  test('Ctrl+T shows the Targets view; Ctrl+L locks', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    await user.keyboard('{Control>}t{/Control}');
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    expect(tab('prod-eu').getAttribute('aria-selected')).toBe('false');
    await user.keyboard('{Control>}l{/Control}');
    expect(calls).toContain('lock_now');
  });

  test('with no system authentication a notice stays under the title bar, on every view', async () => {
    const user = shell(NO_AUTH);
    expect(screen.getByRole('note').textContent).toBe(NO_AUTH_NOTICE);
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    expect(screen.getByRole('note').textContent).toBe(NO_AUTH_NOTICE);
    cleanup();
    shell();
    expect(screen.queryByRole('note')).toBeNull();
  });

  test('on a protected store with no system authentication, the notice says confirmations ask for the lock password', async () => {
    const user = shell({
      ...INFO,
      auth: { ...INFO.auth, available: false, method: null, unavailable: 'no_backend' },
      secrets: {
        mode: 'locked',
        slots: ['password', 'recovery_key'],
        protected: true,
        store: '~/.config/apprafter',
        problem: null,
      },
    });
    expect(screen.getByText(NO_AUTH_PROTECTED_NOTICE)).toBeDefined();
    expect(screen.queryByText(NO_AUTH_NOTICE)).toBeNull();
    // The lock is in effect there (T74): its field opens it whatever the OS offers.
    await waitFor(() =>
      expect(
        (screen.getByRole('button', { name: 'Lock Ctrl+L' }) as HTMLButtonElement).disabled,
      ).toBe(false),
    );
    await user.keyboard('{Control>}l{/Control}');
    expect(calls).toContain('lock_now');
  });

  test('with no system authentication Lock is disabled, and Ctrl+L says why instead', async () => {
    const user = shell(NO_AUTH);
    await screen.findByRole('button', { name: 'Lock Ctrl+L' });
    expect(
      (screen.getByRole('button', { name: 'Lock Ctrl+L' }) as HTMLButtonElement).disabled,
    ).toBe(true);
    await user.keyboard('{Control>}l{/Control}');
    expect(calls).not.toContain('lock_now');
    expect(screen.getByRole('status').textContent).toBe(NO_AUTH_NOTICE);
  });

  test('with Require unlock off, Lock is disabled too, and Ctrl+L says where to turn it on', async () => {
    stored = settings({ lockEnabled: false });
    const user = shell();
    await waitFor(() =>
      expect(
        (screen.getByRole('button', { name: 'Lock Ctrl+L' }) as HTMLButtonElement).disabled,
      ).toBe(true),
    );
    await user.keyboard('{Control>}l{/Control}');
    expect(calls).not.toContain('lock_now');
    expect(screen.getByRole('status').textContent).toBe(
      'The app lock is off: turn on Require unlock in Settings to use it.',
    );
  });

  test('each tab controls its view: a tab panel named by the tab', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    const prod = tab('prod-eu');
    const panel = document.getElementById(prod.getAttribute('aria-controls') ?? '');
    expect(panel?.getAttribute('role')).toBe('tabpanel');
    expect(panel?.getAttribute('aria-labelledby')).toBe(prod.id);
    expect(panel?.contains(screen.getByRole('heading', { level: 1 }))).toBe(true);
  });

  test('while a dialog holds the focus, Ctrl+T and Ctrl+, leave it be; Ctrl+L still locks', async () => {
    summaries = [
      { opId: 1, title: 'Upgrade platform', target: 'prod-eu', state: 'running', startedAtMs: 1 },
    ];
    const user = shell();
    await act(() => refreshList());
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    // A tab's own dialog.
    await user.click(screen.getByRole('button', { name: '1 running' }));
    const sheet = screen.getByRole('dialog', { name: 'Running operations' });
    expect(sheet.contains(document.activeElement)).toBe(true);
    await user.keyboard('{Control>}t{/Control}');
    expect(tab(/prod-eu/).getAttribute('aria-selected')).toBe('true');
    await user.keyboard('{Control>}[Comma]{/Control}');
    expect(screen.queryByRole('dialog', { name: 'Settings' })).toBeNull();
    await user.keyboard('{Escape}');
    // Settings over everything.
    await user.keyboard('{Control>}[Comma]{/Control}');
    expect(await screen.findByRole('dialog', { name: 'Settings' })).toBeDefined();
    await user.keyboard('{Control>}t{/Control}');
    expect(tab(/prod-eu/).getAttribute('aria-selected')).toBe('true');
    expect(screen.getByRole('dialog', { name: 'Settings' })).toBeDefined();
    await user.keyboard('{Control>}l{/Control}');
    expect(calls).toContain('lock_now');
  });

  test('closing Settings cancels the check it started: its scope ends with it', async () => {
    const report = whoamiReport({ status: 'skipped', reason: 'no_ping' });
    let ping = 0;
    clearMocks();
    mockWindows('main');
    mockIPC(
      (cmd, args) => {
        calls.push(cmd);
        if (cmd === 'op_cancel') cancels.push((args as { opId: number }).opId);
        if (cmd === 'op_list') return [];
        if (cmd === 'target_list') return TARGETS;
        if (cmd === 'settings_get') return stored;
        if (cmd === 'whoami') return report;
        if (cmd === 'op_start_whoami') {
          ping = 900_000 + calls.length;
          return ping;
        }
        if (cmd === 'op_subscribe') return { subscription: 1, replay: [] }; // keeps running
        if (cmd === 'plugin:window|is_maximized') return false;
        return null;
      },
      { shouldMockEvents: true },
    );
    const cancels: number[] = [];
    const user = shell();
    await user.keyboard('{Control>}[Comma]{/Control}');
    await user.click(await screen.findByRole('button', { name: 'Verify' }));
    await waitFor(() => expect(calls).toContain('op_subscribe'));
    // Hidden behind nothing and still open, it keeps running.
    expect(cancels).toEqual([]);
    await user.click(
      within(screen.getByRole('dialog', { name: 'Settings' })).getByRole('button', {
        name: 'Close',
      }),
    );
    expect(screen.queryByRole('dialog', { name: 'Settings' })).toBeNull();
    await waitFor(() => expect(cancels).toEqual([ping]));
  });

  test('closing the shown tab shows its neighbour, and the last one the Targets view', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    await user.click(screen.getByRole('button', { name: 'Open a cluster' }));
    await user.click(await screen.findByRole('button', { name: 'Open staging' }));
    await user.click(screen.getByRole('button', { name: 'Close staging' }));
    expect(tab('prod-eu').getAttribute('aria-selected')).toBe('true');
    await user.click(screen.getByRole('button', { name: 'Close prod-eu' }));
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    expect(screen.queryAllByRole('tab')).toHaveLength(0);
  });

  test("a tab's overlay hides with its tab, and is back when the tab is", async () => {
    summaries = [
      { opId: 1, title: 'Upgrade platform', target: 'prod-eu', state: 'running', startedAtMs: 1 },
    ];
    const user = shell();
    await act(() => refreshList());
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    await user.click(screen.getByRole('button', { name: '1 running' }));
    expect(screen.getByRole('dialog', { name: 'Running operations' })).toBeDefined();
    // The tab strip stays in reach of a tab's dialog (the shortcuts do not, see below).
    await user.click(screen.getByRole('button', { name: 'Open a cluster' }));
    expect(screen.queryByRole('dialog', { name: 'Running operations' })).toBeNull();
    await user.click(tab(/prod-eu/));
    expect(
      within(screen.getByRole('dialog', { name: 'Running operations' })).getByText(
        'Upgrade platform',
      ),
    ).toBeDefined();
  });

  test("the sidebar's Security shows the Security view: the store, its targets, the focus on its title; Ctrl+T and Ctrl+L as anywhere", async () => {
    const user = shell();
    const targets = () => screen.getByRole('button', { name: /^Targets/ });
    expect(targets().getAttribute('aria-current')).toBe('page');
    await user.click(await screen.findByRole('button', { name: 'Security' }));
    const title = screen.getByRole('heading', { level: 1, name: 'Security' });
    expect(document.activeElement === title).toBe(true);
    expect(screen.getByText('Target store ~/.config/apprafter')).toBeDefined();
    expect(await screen.findByText('Covers 3 targets: lab, prod-eu, staging')).toBeDefined();
    expect(screen.queryByRole('heading', { name: 'Open a cluster' })).toBeNull();
    expect(screen.getByRole('button', { name: 'Security' }).getAttribute('aria-current')).toBe(
      'page',
    );
    expect(targets().getAttribute('aria-current')).toBeNull();
    await user.keyboard('{Control>}t{/Control}');
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Security' }));
    await user.keyboard('{Control>}l{/Control}');
    expect(calls).toContain('lock_now');
  });

  test('hidden, the Security view reads nothing: no lock_info until it is shown', async () => {
    const user = shell();
    await screen.findByRole('button', { name: 'Open prod-eu' });
    await settleIpc();
    expect(calls).not.toContain('lock_info');
    await user.click(screen.getByRole('button', { name: 'Security' }));
    await waitFor(() => expect(calls.includes('lock_info')).toBe(true));
  });

  // Critic code 5: "each time it is shown", not only the first.
  test('every request to show the Security view reads app_info and lock_info again, shown already or not', async () => {
    const user = await gatedShell();
    await user.click(screen.getByRole('button', { name: 'Security' }));
    await waitFor(() => expect(count('lock_info') >= 1).toBe(true));
    await settleIpc();
    const first = { app: count('app_info'), lock: count('lock_info') };
    // Shown already: the view's own sidebar item asks again.
    await user.click(screen.getByRole('button', { name: 'Security' }));
    await waitFor(() =>
      expect(count('lock_info') > first.lock && count('app_info') > first.app).toBe(true),
    );
    await settleIpc();
    const second = { app: count('app_info'), lock: count('lock_info') };
    await user.keyboard('{Control>}t{/Control}');
    await user.click(screen.getByRole('button', { name: 'Security' }));
    await waitFor(() =>
      expect(count('lock_info') > second.lock && count('app_info') > second.app).toBe(true),
    );
  });

  test('a flow from the Security page opens over every view: the tab strip inert, Ctrl+L still locks', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    await user.click(screen.getByRole('button', { name: 'Security' }));
    await user.click(screen.getByRole('button', { name: 'Protect…' }));
    expect(await screen.findByRole('dialog', { name: 'Protect secrets' })).toBeDefined();
    expect(document.querySelector('.tabstrip')?.hasAttribute('inert')).toBe(true);
    await user.keyboard('{Control>}l{/Control}');
    expect(calls).toContain('lock_now');
  });

  test("Settings' Open Security closes Settings and shows the Security view, the focus on its title", async () => {
    const user = shell();
    await user.keyboard('{Control>}[Comma]{/Control}');
    const settings = await screen.findByRole('dialog', { name: 'Settings' });
    // Settings' body renders once settings_get answers (critic code 12).
    await user.click(await within(settings).findByRole('button', { name: 'Open Security' }));
    expect(screen.queryByRole('dialog', { name: 'Settings' })).toBeNull();
    const title = screen.getByRole('heading', { level: 1, name: 'Security' });
    await waitFor(() => expect(document.activeElement === title).toBe(true));
  });

  // Finding F4 (critic code 1): Settings opened from the Security view's own sidebar keeps that
  // Settings button as its opener, and its close gives the focus back to it. The view does not
  // change, so only the page's new mount (its key) moves the focus on.
  test('Open Security from Settings opened on the Security view itself: the focus goes to the title', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Security' }));
    await user.click(screen.getByRole('button', { name: /^Settings/ }));
    const settings = await screen.findByRole('dialog', { name: 'Settings' });
    await user.click(await within(settings).findByRole('button', { name: 'Open Security' }));
    expect(screen.queryByRole('dialog', { name: 'Settings' })).toBeNull();
    const title = screen.getByRole('heading', { level: 1, name: 'Security' });
    await waitFor(() => expect(document.activeElement === title).toBe(true));
  });

  // Critic m10: reached by Tab, not only by a focus() call.
  test("the Security item is reached by the keyboard from a tab's sidebar, and Enter shows the page", async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    const item = screen.getByRole('button', { name: 'Security' });
    for (let i = 0; i < 60 && document.activeElement !== item; i += 1) {
      await user.tab({ shift: true });
    }
    expect(document.activeElement === item).toBe(true);
    await user.keyboard('{Enter}');
    const title = screen.getByRole('heading', { level: 1, name: 'Security' });
    await waitFor(() => expect(document.activeElement === title).toBe(true));
  });

  // Finding F5 (critic M1): a terminal's `lock off` under the unlocked app changes app_info with
  // no lock transition; the window gaining the focus reads it again.
  test('the window gaining the focus reads app_info again: the store line follows a terminal', async () => {
    appInfoNow = { ...INFO, secrets: { ...INFO.secrets, mode: 'locked', protected: true } };
    const user = await gatedShell();
    await user.click(screen.getByRole('button', { name: 'Security' }));
    expect(await screen.findByText('Secrets are encrypted; unlocking opens them.')).toBeDefined();
    await settleIpc();
    const lockInfos = count('lock_info');
    appInfoNow = INFO;
    await emit('tauri://focus', null);
    expect(
      await screen.findByText("Secrets are in files; the lock only covers this app's window."),
    ).toBeDefined();
    // The page is shown, so its report is read again too.
    await waitFor(() => expect(count('lock_info') > lockInfos).toBe(true));
  });

  test("the Targets page's Security › shows the Security view (WI-476)", async () => {
    const user = shell();
    expect(screen.getByText('Target store ~/.config/apprafter · Secrets: in files')).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Security page' }));
    const title = screen.getByRole('heading', { level: 1, name: 'Security' });
    await waitFor(() => expect(document.activeElement === title).toBe(true));
  });

  // Finding F5 on the line that stays on screen: a terminal's `lock off` under the app.
  test("the Targets page's line follows a terminal once the window has the focus again", async () => {
    appInfoNow = { ...INFO, secrets: { ...INFO.secrets, mode: 'locked', protected: true } };
    await gatedShell();
    expect(screen.getByText('Target store ~/.config/apprafter · Secrets: protected')).toBeDefined();
    await settleIpc();
    appInfoNow = INFO;
    await emit('tauri://focus', null);
    expect(
      await screen.findByText('Target store ~/.config/apprafter · Secrets: in files'),
    ).toBeDefined();
  });
});

/** Every `cmd` the page sends from now on: its args, and the answer the mock gives. */
function watch(cmd: string): { args: Record<string, unknown>[]; answers: Promise<unknown>[] } {
  const internals = (
    window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown, options?: unknown) => unknown };
    }
  ).__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  const seen = { args: [] as Record<string, unknown>[], answers: [] as Promise<unknown>[] };
  internals.invoke = (c, args, options) => {
    const answer = invoke(c, args, options);
    if (c === cmd) {
      seen.args.push({ ...(args as Record<string, unknown>) });
      seen.answers.push(Promise.resolve(answer));
    }
    return answer;
  };
  return seen;
}

describe('Shell, the Target section on the mock IPC', () => {
  beforeEach(async () => {
    clearMocks();
    installMockIpc({ opDelayMs: 0 });
    await api.unlock();
  });

  /** staging's Target screen with the renew confirm open: the plan Rust holds, with a token. */
  async function renewConfirmOnStaging(user: ReturnType<typeof userEvent.setup>) {
    await user.click(await screen.findByRole('button', { name: 'Open staging' }));
    await user.click(screen.getByRole('button', { name: 'Target' }));
    await user.click(await screen.findByRole('button', { name: 'Renew' }));
    const form = screen.getByRole('dialog', { name: 'Renew API token' });
    await user.type(within(form).getByLabelText('Hetzner Cloud token'), 'k'.repeat(64));
    await user.click(within(form).getByRole('button', { name: 'Continue' }));
    await screen.findByRole('dialog', { name: 'Renew the API token of staging?' });
  }

  test('closing a tab discards the plan its confirm holds, and the token with it (review #13)', async () => {
    const plans = watch('op_plan_target_renew');
    const discards = watch('op_discard');
    const user = shell();
    await renewConfirmOnStaging(user);
    const view = (await plans.answers[0]) as PlanView;
    // Hidden with its tab, the confirm keeps its plan: a hidden Activity runs its effect
    // cleanups, so nothing there may discard it.
    await user.click(screen.getByRole('button', { name: 'Open a cluster' }));
    await user.click(tab('staging'));
    expect(screen.getByRole('dialog', { name: 'Renew the API token of staging?' })).toBeDefined();
    expect(discards.args).toEqual([]);
    await user.click(screen.getByRole('button', { name: 'Close staging' }));
    await waitFor(() => expect(discards.args).toEqual([{ opId: view.opId }]));
  });

  test('a plan that ran, or that its confirm discarded, is discarded once: the tab closing adds none', async () => {
    const plans = watch('op_plan_target_renew');
    const discards = watch('op_discard');
    const user = shell();
    await renewConfirmOnStaging(user);
    const cancelled = (await plans.answers[0]) as PlanView;
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(discards.args).toEqual([{ opId: cancelled.opId }]));
    await user.click(await screen.findByRole('button', { name: 'Renew' }));
    const form = screen.getByRole('dialog', { name: 'Renew API token' });
    await user.type(within(form).getByLabelText('Hetzner Cloud token'), 'k'.repeat(64));
    await user.click(within(form).getByRole('button', { name: 'Continue' }));
    const confirm = await screen.findByRole('dialog', { name: 'Renew the API token of staging?' });
    const ran = (await plans.answers[1]) as PlanView;
    await user.click(within(confirm).getByRole('button', { name: 'Renew' }));
    expect(await screen.findByText(/Token renewed/)).toBeDefined();
    // The run's end discards its record (plans.ts), once.
    await waitFor(() =>
      expect(discards.args).toEqual([{ opId: cancelled.opId }, { opId: ran.opId }]),
    );
    await user.click(screen.getByRole('button', { name: 'Close staging' }));
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(discards.args).toEqual([{ opId: cancelled.opId }, { opId: ran.opId }]);
  });

  test('the Target section is the Target screen, and the sidebar says provider · region · tier', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open prod-eu' }));
    expect(await screen.findByText('hetzner-cloud · nbg1 · T2')).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Target' }));
    expect(pageTitle()).toBe('Target');
    expect(await screen.findByRole('region', { name: 'Target' })).toBeDefined();
    expect(screen.queryByRole('heading', { name: /arrives in D\.3/ })).toBeNull();
  });

  // WI-458: a target with a tab whose files became unreadable (edited in a terminal) is removed
  // from its card on the Targets view, and its tab closes as a removal from the tab's screen does.
  test('removing an unreadable target from the Targets view closes its tab', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open lab' }));
    expect(tab('lab').getAttribute('aria-selected')).toBe('true');
    const internals = (
      window as unknown as {
        __TAURI_INTERNALS__: {
          invoke: (cmd: string, args?: unknown, options?: unknown) => unknown;
        };
      }
    ).__TAURI_INTERNALS__;
    const invoke = internals.invoke;
    // The store now lists lab as unreadable, until it is removed.
    internals.invoke = async (c, args, options) => {
      const answer = await invoke(c, args, options);
      if (c !== 'target_list') return answer;
      const list = answer as TargetListReport;
      const lab = list.targets.find((t) => t.name === 'lab');
      if (lab === undefined) return list;
      return {
        ...list,
        targets: list.targets.filter((t) => t !== lab),
        unreadable: [
          ...list.unreadable,
          {
            name: 'lab',
            error: {
              code: 'apprafter::target::invalid_config',
              message: 'target config at ~/.config/apprafter/targets/lab/config.yaml: bad',
              help: null,
              causes: [],
              fields: { target: 'lab' },
            },
          },
        ],
      };
    };
    await user.keyboard('{Control>}t{/Control}');
    const card = await screen.findByRole('article', { name: 'lab' });
    await user.click(within(card).getByRole('button', { name: 'Remove lab' }));
    const remove = await screen.findByRole('dialog', { name: 'Remove target lab?' });
    await user.type(within(remove).getByLabelText(/Type lab to confirm/), 'lab');
    await user.click(within(remove).getByRole('button', { name: 'Remove target' }));
    await waitFor(() => expect(screen.queryByRole('tab', { name: 'lab' })).toBeNull());
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
  });

  test('a rename rebinds the tab and its sidebar line; a remove closes the tab', async () => {
    const user = shell();
    await user.click(await screen.findByRole('button', { name: 'Open staging' }));
    await user.click(screen.getByRole('button', { name: 'Target' }));
    await user.click(await screen.findByRole('button', { name: 'Rename' }));
    const form = screen.getByRole('dialog', { name: 'Rename target' });
    await user.clear(within(form).getByLabelText('New name'));
    await user.type(within(form).getByLabelText('New name'), 'staging-2');
    await user.click(within(form).getByRole('button', { name: 'Continue' }));
    const confirm = await screen.findByRole('dialog', { name: 'Rename staging to staging-2?' });
    await user.click(within(confirm).getByRole('button', { name: 'Rename' }));
    await waitFor(() => expect(tab('staging-2').getAttribute('aria-selected')).toBe('true'));
    expect(screen.queryByRole('tab', { name: 'staging' })).toBeNull();
    expect(pageTitle()).toBe('Target');
    // Review #14: the screen keeps its cards across the rename (no spinner while the new name
    // is read), so the focus goes back to the Rename button that started it.
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Rename' }));
    expect(await screen.findByText('hetzner-cloud · fsn1 · T1')).toBeDefined();
    expect((await screen.findByRole('group', { name: 'Name' })).textContent).toContain('staging-2');

    await user.click(screen.getByRole('button', { name: 'Remove…' }));
    const remove = await screen.findByRole('dialog', { name: 'Remove target staging-2?' });
    await user.type(within(remove).getByLabelText(/Type staging-2 to confirm/), 'staging-2');
    await user.click(within(remove).getByRole('button', { name: 'Remove target' }));
    await waitFor(() => expect(screen.queryAllByRole('tab')).toHaveLength(0));
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    expect(await screen.findByText(/Removed staging-2 from this computer/)).toBeDefined();
  });
});
