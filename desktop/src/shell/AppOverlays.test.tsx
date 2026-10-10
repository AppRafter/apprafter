// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The app's own overlays in the real Shell under the LockGate, on D.3d's mock engine: the
// add-target wizard and Doctor open over every view; while one is open the tab strip, the views
// and every shortcut but Lock are inert, the caption buttons live; a lock takes them with what
// was typed; closing the wizard discards its draft, once.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider } from '../components/Toast';
import * as api from '../ipc/api';
import { resetEndedAway } from '../ipc/away';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import { resetLifecycle } from '../ipc/lifecycle';
import { installMockIpc } from '../ipc/mock';
import { resetOperations } from '../ipc/operations';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo } from '../test/fixtures';
import { settleIpc } from '../test/settle';
import { LockGate } from './LockGate';
import { Shell } from './Shell';

const TOKEN = 'A1'.repeat(32);

beforeEach(async () => {
  installMockIpc({ os: 'windows', opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetLifecycle();
  resetOperations();
  resetEndedAway();
  clearMocks();
});

/** Every `cmd` the page sends from now on, with its arguments. */
function watch(cmd: string): Record<string, unknown>[] {
  const internals = (
    window as unknown as {
      __TAURI_INTERNALS__: { invoke: (c: string, args?: unknown, options?: unknown) => unknown };
    }
  ).__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  const seen: Record<string, unknown>[] = [];
  internals.invoke = (c, args, options) => {
    if (c === cmd) seen.push({ ...(args as Record<string, unknown>) });
    return invoke(c, args, options);
  };
  return seen;
}

function app() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo({ os: 'windows' })}>
        <ToastProvider>
          <LockGate>
            <Shell />
          </LockGate>
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}

const tab = (name: string) => screen.getByRole('tab', { name });

/** A tab for staging, then the Targets view and its Add target card: the wizard, verified. */
async function wizardWithDraft(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: 'Open staging' }));
  await user.click(screen.getByRole('button', { name: 'Open a cluster' }));
  await user.click(await screen.findByRole('button', { name: /Add target/ }));
  const wizard = await screen.findByRole('dialog', { name: 'Add target' });
  await user.type(within(wizard).getByLabelText('API token'), TOKEN);
  await user.click(within(wizard).getByRole('button', { name: 'Verify and continue' }));
  await waitFor(() =>
    expect(document.querySelector('[aria-current="step"]')?.textContent ?? '').toContain('Machine'),
  );
  return wizard;
}

test('while the wizard is open, the tab strip and Ctrl+T / Ctrl+, do nothing; Ctrl+L locks', async () => {
  const user = app();
  await wizardWithDraft(user);
  const strip = document.querySelector('.tabstrip') as HTMLElement;
  expect(strip.hasAttribute('inert')).toBe(true);
  // The caption buttons stay live: the title bar itself is not inert.
  expect(screen.getByRole('button', { name: 'Minimize' }).closest('[inert]')).toBeNull();
  // With the focus outside the wizard too (on a caption button, which stays live), the
  // shortcuts other than Lock wait.
  screen.getByRole('button', { name: 'Minimize' }).focus();
  await user.keyboard('{Control>}t{/Control}');
  await user.keyboard('{Control>}[Comma]{/Control}');
  expect(screen.queryByRole('dialog', { name: 'Settings' })).toBeNull();
  expect(screen.getByRole('dialog', { name: 'Add target' })).toBeDefined();
  await user.keyboard('{Control>}l{/Control}');
  await waitFor(() =>
    expect(screen.queryByRole('dialog', { name: 'Add target' }) === null).toBe(true),
  );
  expect(await screen.findByRole('button', { name: /Unlock/ })).toBeDefined();
});

test('while the wizard is open, no tab can be shown: a click on one reaches nothing', async () => {
  const user = app();
  await wizardWithDraft(user);
  expect(tab('staging').getAttribute('aria-selected')).toBe('false');
  // happy-dom does not honour inert, so the click lands; the Shell still ignores it.
  await user.click(tab('staging'));
  expect(tab('staging').getAttribute('aria-selected')).toBe('false');
  await user.click(screen.getByRole('button', { name: 'Close staging' }));
  expect(screen.queryAllByRole('tab')).toHaveLength(1);
  expect(screen.getByRole('dialog', { name: 'Add target' })).toBeDefined();
});

test('a lock takes the wizard with what was typed; Rust drops its draft; an unlock brings neither back', async () => {
  const user = app();
  const discards = watch('target_draft_discard');
  await wizardWithDraft(user);
  await act(async () => {
    await api.lockNow();
  });
  await waitFor(() =>
    expect(screen.queryByRole('dialog', { name: 'Add target' }) === null).toBe(true),
  );
  expect(document.body.textContent?.includes(TOKEN)).toBe(false);
  await act(async () => {
    await api.unlock();
  });
  expect(await screen.findByRole('button', { name: 'Open a cluster' })).toBeDefined();
  expect(screen.queryByRole('dialog', { name: 'Add target' })).toBeNull();
  // The lock's own discard of the draft was refused as locked: Rust drops it on the lock.
  const draftId = discards[0]?.draftId as number | undefined;
  expect(draftId).toBeDefined();
  const refused = await api.opStartMachineCatalogue({ kind: 'draft', draftId: draftId ?? -1 }).then(
    () => null,
    (e: unknown) => api.uiErrorOf(e).code,
  );
  expect(refused).toBe(DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND);
});

test('closing the wizard discards its draft, once', async () => {
  const user = app();
  const discards = watch('target_draft_discard');
  const wizard = await wizardWithDraft(user);
  await user.click(within(wizard).getByRole('button', { name: 'Close' }));
  expect(screen.queryByRole('dialog', { name: 'Add target' })).toBeNull();
  await settleIpc();
  expect(discards).toHaveLength(1);
  // The tab strip is live again.
  expect((document.querySelector('.tabstrip') as HTMLElement).hasAttribute('inert')).toBe(false);
});

test('a lock takes the doctor too', async () => {
  const user = app();
  await user.click(await screen.findByRole('button', { name: 'Open staging' }));
  await user.click(screen.getByRole('button', { name: 'Run doctor' }));
  expect(await screen.findByRole('dialog', { name: 'Doctor · staging' })).toBeDefined();
  expect((document.querySelector('.tabstrip') as HTMLElement).hasAttribute('inert')).toBe(true);
  await user.keyboard('{Control>}l{/Control}');
  await waitFor(() =>
    expect(screen.queryByRole('dialog', { name: 'Doctor · staging' }) === null).toBe(true),
  );
  await act(async () => {
    await api.unlock();
  });
  expect(await screen.findByRole('button', { name: 'Open a cluster' })).toBeDefined();
  expect(screen.queryByRole('dialog', { name: 'Doctor · staging' })).toBeNull();
});
