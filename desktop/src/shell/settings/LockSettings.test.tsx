// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, describe, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import * as api from '../../ipc/api';
import { resetEndedAway } from '../../ipc/away';
import type { SecretsMode } from '../../ipc/generated/SecretsMode';
import { resetHeldPlans } from '../../ipc/heldPlans';
import { installMockIpc } from '../../ipc/mock';
import { MOCK_RECOVERY_KEY, MOCK_STORE_PASSWORD } from '../../ipc/mock/lock';
import { resetOperations } from '../../ipc/operations';
import {
  ACTION_LABELS,
  BACKED_OUT_TOAST,
  FINISH_TITLE,
  PROTECT_TITLE,
  RESTART_NOTE,
  STOP_FINISH_TITLE,
  STOP_TITLE,
  STOPPED_TOAST,
  STORE_PROBLEM_SUB,
  STORE_SECRET_FIELD,
} from '../../state/lockText';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo, STORE_PROBLEMS, settings as savedSettings } from '../../test/fixtures';
import { settleIpc } from '../../test/settle';
import { PlatformGate } from '../PlatformGate';
import { SettingsDialog } from '../SettingsDialog';
import { LockFlow, LockSettings } from './LockSettings';

afterEach(async () => {
  cleanup();
  await settleIpc();
  // As its siblings that run real plans on the mock (UI verifier minor 6).
  resetOperations();
  resetHeldPlans();
  resetEndedAway();
  clearMocks();
});

/** Settings under the platform gate, as the app has it, on the mock with the store `secrets`. */
async function open(secrets: SecretsMode) {
  installMockIpc({ opDelayMs: 0, secrets });
  if (secrets === 'files') await api.unlock();
  else await api.unlockWithPassword(MOCK_STORE_PASSWORD);
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

const settings = () => screen.getByRole('dialog', { name: 'Settings' });

describe('Settings › Lock on the commands', () => {
  test('the section reads lock_info: the limits of this computer', async () => {
    await open('files');
    expect(await within(settings()).findByText(/Yama ptrace_scope is 0/)).toBeDefined();
    expect(within(settings()).queryByText('Security')).toBeNull();
  });

  test('Protect runs from Settings, beside it: the key saved, then typed back (O12)', async () => {
    const user = await open('files');
    await user.click(within(settings()).getByRole('button', { name: ACTION_LABELS.protect }));
    const wizard = await screen.findByRole('dialog', { name: PROTECT_TITLE });
    expect(settings().contains(wizard)).toBe(false);
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
  });

  // Nothing is called encrypted that is not (critic B finding 8): each state whose lock state
  // cannot be used, in the core's words, with no way on but its repair.
  test.each(STORE_PROBLEMS)(
    "the section says a store that cannot be used, in the core's words: %s",
    async (_state, secrets) => {
      installMockIpc({ opDelayMs: 0 });
      await api.unlock();
      render(
        <QueryClientProvider client={createQueryClient()}>
          <PlatformContext value={appInfo({ secrets })}>
            <ToastProvider>
              <LockSettings settings={savedSettings()} onAction={mock()} />
            </ToastProvider>
          </PlatformContext>
        </QueryClientProvider>,
      );
      expect(await screen.findByText(secrets.problem?.message ?? '?')).toBeDefined();
      expect(screen.getByText(STORE_PROBLEM_SUB)).toBeDefined();
      // protectSub('locked') is the one line of the section that says they are encrypted.
      expect(screen.queryByText(/^Secrets are encrypted/)).toBeNull();
      for (const label of Object.values(ACTION_LABELS)) {
        expect(screen.queryByRole('button', { name: label })).toBeNull();
      }
    },
  );

  test('Stop protecting asks for the store secret, then the store is in files again', async () => {
    const user = await open('locked');
    await user.click(await within(settings()).findByRole('button', { name: ACTION_LABELS.stop }));
    const confirm = await screen.findByRole('dialog', { name: STOP_TITLE });
    await user.type(within(confirm).getByLabelText(STORE_SECRET_FIELD), MOCK_STORE_PASSWORD);
    await user.click(within(confirm).getByRole('button', { name: 'Stop protecting' }));
    expect(await screen.findByText(STOPPED_TOAST)).toBeDefined();
    await waitFor(() =>
      expect(within(settings()).getByRole('button', { name: ACTION_LABELS.protect })).toBeDefined(),
    );
  });

  test('Stop protecting an interrupted Protect backs it out, and the toast says so (UI verifier M3)', async () => {
    const user = await open('locking');
    await user.click(await within(settings()).findByRole('button', { name: ACTION_LABELS.stop }));
    const confirm = await screen.findByRole('dialog', { name: STOP_TITLE });
    await user.type(within(confirm).getByLabelText(STORE_SECRET_FIELD), MOCK_STORE_PASSWORD);
    await user.click(within(confirm).getByRole('button', { name: 'Stop protecting' }));
    expect(await screen.findByText(BACKED_OUT_TOAST)).toBeDefined();
    expect(screen.queryByText(STOPPED_TOAST)).toBeNull();
  });

  test('an interrupted Protect finishes with both secrets; an interrupted Stop with its confirm', async () => {
    let user = await open('locking');
    await user.click(await within(settings()).findByRole('button', { name: ACTION_LABELS.finish }));
    expect(await screen.findByRole('dialog', { name: FINISH_TITLE })).toBeDefined();
    cleanup();
    await settleIpc();
    clearMocks();
    user = await open('unlocking');
    await user.click(await within(settings()).findByRole('button', { name: ACTION_LABELS.finish }));
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
