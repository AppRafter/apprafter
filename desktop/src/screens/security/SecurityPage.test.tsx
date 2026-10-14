// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Security page on the mock IPC (WI-476; D.4a §3.14): the store and every target it covers,
// its report, and the store lock's flows — moved here with their tests from Settings › Lock
// (LockSettings.test.tsx at cb9c74e0), each run from the page now, as the app's own overlays.
import { afterEach, describe, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import * as api from '../../ipc/api';
import { resetEndedAway } from '../../ipc/away';
import type { LockOffPlan } from '../../ipc/generated/LockOffPlan';
import type { SecretsMode } from '../../ipc/generated/SecretsMode';
import type { UiError } from '../../ipc/generated/UiError';
import { resetHeldPlans } from '../../ipc/heldPlans';
import { resetLifecycle, sessionLocked } from '../../ipc/lifecycle';
import { installMockIpc } from '../../ipc/mock';
import { MOCK_RECOVERY_KEY, MOCK_STORE_PASSWORD } from '../../ipc/mock/lock';
import { resetOperations } from '../../ipc/operations';
import { PlatformGate } from '../../shell/PlatformGate';
import {
  ACTION_LABELS,
  BACKED_OUT_TOAST,
  FINISH_TITLE,
  PROTECT_TITLE,
  RESTART_NOTE,
  STOP_FINISH_TITLE,
  STOP_TITLE,
  STOPPED_TOAST,
  STORE_NOT_PRIVATE_SUB,
  STORE_PROBLEM_SUB,
  STORE_SECRET_FIELD,
} from '../../state/lockText';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo, STORE_NOT_PRIVATE, STORE_PROBLEMS } from '../../test/fixtures';
import { renderScreen, ScreenHost } from '../../test/screens';
import { settleIpc } from '../../test/settle';
import { LockFlow } from './LockFlow';
import { SecurityPage } from './SecurityPage';

afterEach(async () => {
  cleanup();
  await settleIpc();
  // As its siblings that run real plans on the mock (UI verifier minor 6); a test that ends the
  // session (sessionLocked) leaves a fresh one.
  resetOperations();
  resetHeldPlans();
  resetEndedAway();
  resetLifecycle();
  clearMocks();
});

type Invoke = (cmd: string, args?: unknown, options?: unknown) => unknown;
const internals = () =>
  (window as unknown as { __TAURI_INTERNALS__: { invoke: Invoke } }).__TAURI_INTERNALS__;

/** The arguments of every `cmd` sent from now on (a spy over the mock's invoke). */
function sent(cmd: string): unknown[] {
  const at = internals();
  const invoke = at.invoke;
  const seen: unknown[] = [];
  at.invoke = (c, args, options) => {
    if (c === cmd) seen.push(args);
    return invoke(c, args, options);
  };
  return seen;
}

/** What `cmd` answers from now on (a spy over the mock's invoke). */
function answers(cmd: string): unknown[] {
  const at = internals();
  const invoke = at.invoke;
  const seen: unknown[] = [];
  at.invoke = (c, args, options) => {
    const answer = invoke(c, args, options);
    if (c === cmd) {
      Promise.resolve(answer).then(
        (value) => seen.push(value),
        () => {},
      );
    }
    return answer;
  };
  return seen;
}

/** `cmd` refused with `error` from now on (over the mock's invoke). */
function refuse(cmd: string, error: UiError): void {
  const at = internals();
  const invoke = at.invoke;
  at.invoke = (c, args, options) => (c === cmd ? Promise.reject(error) : invoke(c, args, options));
}

/**
 * The page as the shell hosts it, under the platform gate, on the mock with the store `secrets`;
 * `before` runs on the unlocked mock just before the page renders.
 */
async function open(secrets: SecretsMode = 'files', before: () => void = () => {}) {
  installMockIpc({ opDelayMs: 0, secrets });
  if (secrets === 'files') await api.unlock();
  else await api.unlockWithPassword(MOCK_STORE_PASSWORD);
  before();
  const appInfos = sent('app_info');
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformGate>
        <ToastProvider>
          <ScreenHost>
            <SecurityPage />
          </ScreenHost>
          <ToastViewport />
        </ToastProvider>
      </PlatformGate>
    </QueryClientProvider>,
  );
  await screen.findByRole('heading', { level: 1, name: 'Security' });
  return { user: userEvent.setup(), appInfos };
}

/** The page's own view: the flows open beside it, never in it. */
const view = () =>
  screen.getByRole('heading', { level: 1, name: 'Security' }).closest('.view') as HTMLElement;

describe('the Security page (WI-476)', () => {
  test('it names the store and every target it covers, the unreadable one too', async () => {
    await open();
    expect(within(view()).getByText('Target store ~/.config/apprafter')).toBeDefined();
    expect(
      await within(view()).findByText('Covers 4 targets: broken, lab, prod-eu, staging'),
    ).toBeDefined();
  });

  test('shown, it reads app_info again, and the focus starts at its title', async () => {
    const { appInfos } = await open();
    // The gate's read, then the page's own.
    await waitFor(() => expect(appInfos.length >= 2).toBe(true));
    const title = screen.getByRole('heading', { level: 1, name: 'Security' });
    expect(document.activeElement === title).toBe(true);
  });

  // Critic code 6: target_list's error at the page's level, not only in StoreScope.
  test('targets that cannot be listed: the page still names the store, and says why', async () => {
    const error: UiError = {
      code: 'apprafter::io',
      message: 'the targets cannot be read',
      help: null,
      causes: [],
      fields: {},
    };
    await open('files', () => refuse('target_list', error));
    expect(within(view()).getByText('Target store ~/.config/apprafter')).toBeDefined();
    expect(await within(view()).findByText('the targets cannot be read')).toBeDefined();
  });

  // Moved from LockSettings.test.tsx ("the section reads lock_info: the limits of this computer").
  test('the page reads lock_info: the limits of this computer', async () => {
    await open();
    expect(await within(view()).findByText(/Yama ptrace_scope is 0/)).toBeDefined();
  });

  // Moved ("Protect runs from Settings, beside it: …"): from the page, beside its view.
  test('Protect runs from the page, over every view: the key saved, then typed back (O12)', async () => {
    const { user } = await open();
    await user.click(within(view()).getByRole('button', { name: ACTION_LABELS.protect }));
    const wizard = await screen.findByRole('dialog', { name: PROTECT_TITLE });
    expect(view().contains(wizard)).toBe(false);
    await user.click(within(wizard).getByRole('button', { name: 'Continue' }));
    await user.type(within(wizard).getByLabelText('Lock password'), MOCK_STORE_PASSWORD);
    await user.type(within(wizard).getByLabelText('Repeat the lock password'), MOCK_STORE_PASSWORD);
    await user.click(within(wizard).getByRole('button', { name: 'Continue' }));
    await user.click(await within(wizard).findByRole('button', { name: 'I saved it' }));
    // Typed from where it was saved: the mock, as Rust, takes it only once told it is saved.
    await user.type(
      within(wizard).getByLabelText('Type the whole recovery key to confirm you saved it'),
      MOCK_RECOVERY_KEY.replace('\n', ' '),
    );
    await user.click(within(wizard).getByRole('button', { name: 'Protect' }));
    expect(
      await screen.findByText('Secrets protected. Unlocking AppRafter now opens them.'),
    ).toBeDefined();
    await waitFor(() =>
      expect(within(view()).queryByRole('button', { name: ACTION_LABELS.stop }) !== null).toBe(
        true,
      ),
    );
  });

  // Moved: nothing is called encrypted that is not (critic B finding 8).
  test.each(STORE_PROBLEMS)(
    "the page says a store that cannot be used, in the core's words: %s",
    async (_state, secrets) => {
      installMockIpc({ opDelayMs: 0 });
      await api.unlock();
      renderScreen(<SecurityPage />, { info: appInfo({ secrets }) });
      expect(await screen.findByText(secrets.problem?.message ?? '?')).toBeDefined();
      expect(screen.getByText(STORE_PROBLEM_SUB)).toBeDefined();
      // protectSub('locked') is the one line of the page that says they are encrypted.
      expect(screen.queryByText(/^Secrets are encrypted/)).toBeNull();
      for (const label of Object.values(ACTION_LABELS)) {
        expect(screen.queryByRole('button', { name: label })).toBeNull();
      }
    },
  );

  // The controller's decision on critic M7 (WI-475): a store other accounts can change is said in
  // the core's words, with the fix, and nothing the core would refuse is offered; it is never
  // called protected, nor in files.
  test.each(STORE_NOT_PRIVATE)(
    'a store other accounts can change, %s: the core’s why and fix, and no Protect, Finish or Stop',
    async (_state, secrets) => {
      installMockIpc({ opDelayMs: 0 });
      await api.unlock();
      renderScreen(<SecurityPage />, { info: appInfo({ secrets }) });
      const panel = await screen.findByRole('alert');
      expect(panel.textContent).toContain(secrets.problem?.message ?? '?');
      expect(panel.textContent).toContain(secrets.problem?.help ?? '?');
      expect(screen.getByText(STORE_NOT_PRIVATE_SUB)).toBeDefined();
      expect(screen.queryByText(STORE_PROBLEM_SUB)).toBeNull();
      expect(screen.queryByText(/^Secrets are (encrypted|in files)/)).toBeNull();
      for (const label of Object.values(ACTION_LABELS)) {
        expect(screen.queryByRole('button', { name: label })).toBeNull();
      }
    },
  );

  test('Stop protecting asks for the store secret, then the store is in files again', async () => {
    const { user } = await open('locked');
    await user.click(await within(view()).findByRole('button', { name: ACTION_LABELS.stop }));
    const confirm = await screen.findByRole('dialog', { name: STOP_TITLE });
    await user.type(within(confirm).getByLabelText(STORE_SECRET_FIELD), MOCK_STORE_PASSWORD);
    await user.click(within(confirm).getByRole('button', { name: 'Stop protecting' }));
    expect(await screen.findByText(STOPPED_TOAST)).toBeDefined();
    await waitFor(() =>
      expect(within(view()).queryByRole('button', { name: ACTION_LABELS.protect }) !== null).toBe(
        true,
      ),
    );
  });

  test('Stop protecting an interrupted Protect backs it out, and the toast says so (UI verifier M3)', async () => {
    const { user } = await open('locking');
    await user.click(await within(view()).findByRole('button', { name: ACTION_LABELS.stop }));
    const confirm = await screen.findByRole('dialog', { name: STOP_TITLE });
    await user.type(within(confirm).getByLabelText(STORE_SECRET_FIELD), MOCK_STORE_PASSWORD);
    await user.click(within(confirm).getByRole('button', { name: 'Stop protecting' }));
    expect(await screen.findByText(BACKED_OUT_TOAST)).toBeDefined();
    expect(screen.queryByText(STOPPED_TOAST)).toBeNull();
  });

  // Critic code 7: the plan is held by the flow's own overlay now, not by Settings' scope. The
  // confirm's Cancel discards it itself and releases the hold, so the overlay's close, which
  // ends that scope, discards nothing twice.
  test('Cancel on Stop protecting discards its plan once, and the overlay goes with it', async () => {
    const { user } = await open('locked');
    const planned = answers('op_plan_lock_off');
    const discarded = sent('op_discard');
    await user.click(await within(view()).findByRole('button', { name: ACTION_LABELS.stop }));
    const confirm = await screen.findByRole('dialog', { name: STOP_TITLE });
    await user.click(within(confirm).getByRole('button', { name: 'Cancel' }));
    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: STOP_TITLE }) === null).toBe(true),
    );
    await settleIpc();
    expect(discarded).toEqual([{ opId: (planned[0] as LockOffPlan).plan.opId }]);
  });

  // The lock ends the session's scopes, the overlay's under it: the plan goes, never run.
  test('a lock while Stop protecting waits discards its plan, unrun', async () => {
    const { user } = await open('locked');
    const planned = answers('op_plan_lock_off');
    const discarded = sent('op_discard');
    await user.click(await within(view()).findByRole('button', { name: ACTION_LABELS.stop }));
    await screen.findByRole('dialog', { name: STOP_TITLE });
    expect(discarded).toEqual([]);
    act(() => {
      sessionLocked();
    });
    await waitFor(() => expect(discarded.length === 1).toBe(true));
    expect(discarded).toEqual([{ opId: (planned[0] as LockOffPlan).plan.opId }]);
    expect(screen.queryByRole('dialog', { name: STOP_TITLE }) === null).toBe(true);
  });

  test('an interrupted Protect finishes with both secrets; an interrupted Stop with its confirm', async () => {
    let { user } = await open('locking');
    await user.click(await within(view()).findByRole('button', { name: ACTION_LABELS.finish }));
    expect(await screen.findByRole('dialog', { name: FINISH_TITLE })).toBeDefined();
    cleanup();
    await settleIpc();
    clearMocks();
    ({ user } = await open('unlocking'));
    await user.click(await within(view()).findByRole('button', { name: ACTION_LABELS.finish }));
    expect(await screen.findByRole('dialog', { name: STOP_FINISH_TITLE })).toBeDefined();
  });

  test('an interrupted Protect that wrote no vault starts over, its old key void', async () => {
    installMockIpc({ opDelayMs: 0 });
    const info = appInfo({
      secrets: {
        mode: 'locking',
        slots: [],
        protected: false,
        store: '~/.config/apprafter',
        problem: null,
      },
    });
    render(
      <QueryClientProvider client={createQueryClient()}>
        <PlatformContext value={info}>
          <ToastProvider>
            <LockFlow action="finish" onClose={mock()} />
          </ToastProvider>
        </PlatformContext>
      </QueryClientProvider>,
    );
    expect(await screen.findByRole('dialog', { name: PROTECT_TITLE })).toBeDefined();
    expect(screen.getByText(RESTART_NOTE)).toBeDefined();
  });
});
