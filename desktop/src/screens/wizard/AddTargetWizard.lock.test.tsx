// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Spec §4.3 / overview §6.3: locked, the shell is unmounted, and the add-target wizard with it —
// with what was typed. A read it was running is cancelled from here too (Rust cancels reads on a
// lock as well; the page's cancel is then refused as locked, which must not be reported).
import { afterEach, beforeEach, expect, spyOn, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { ToastProvider } from '../../components/Toast';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import { LOCK_CHANGED } from '../../ipc/generated/events';
import { resetOperations } from '../../ipc/operations';
import { LockGate } from '../../shell/LockGate';
import { useOverlay, ViewFrame } from '../../shell/ViewFrame';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo, lockState } from '../../test/fixtures';
import { AddTargetWizard } from './AddTargetWizard';

const TOKEN = 'A1'.repeat(32);
/** What a locked Rust still answers: the lock screen's own commands. */
const ALLOWED_LOCKED = [
  'lock_status',
  'app_info',
  'settings_get',
  'unlock',
  'quit',
  'window_ready',
  'plugin:window|is_maximized',
];
let calls: string[];
let locked: boolean;
/** How the verify ends: running, or verified as draft 7 (then the catalogue read runs). */
let verifyEnds: boolean;
/** The wizard opens in the first shell only: after the unlock the shell mounts afresh. */
let opened: boolean;

beforeEach(() => {
  calls = [];
  locked = false;
  verifyEnds = false;
  opened = false;
  mockWindows('main');
  mockIPC(
    (cmd, args) => {
      calls.push(cmd);
      if (cmd === 'lock_status') return lockState({ locked });
      if (locked && !cmd.startsWith('plugin:') && !ALLOWED_LOCKED.includes(cmd)) {
        return Promise.reject({
          code: DESKTOP_ERROR_CODES.LOCKED,
          message: 'locked',
          help: null,
          causes: [],
          fields: {},
        });
      }
      if (cmd === 'op_start_verify_token') return 41;
      if (cmd === 'op_start_machine_catalogue') return 42;
      if (cmd === 'op_subscribe') {
        const opId = (args as { opId: number }).opId;
        // The verify keeps running unless it is to end; the catalogue read keeps running.
        const replay =
          opId === 41 && verifyEnds
            ? [
                {
                  kind: 'finished',
                  outcome: { status: 'completed', result: { draftId: 7, elapsedMs: 182 } },
                },
              ]
            : [];
        return { subscription: opId, replay };
      }
      return null;
    },
    { shouldMockEvents: true },
  );
});
afterEach(() => {
  cleanup();
  resetOperations();
  clearMocks();
});

function renderShell() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <LockGate>
          <ToastProvider>
            <ViewFrame>
              <OpenWizard />
            </ViewFrame>
          </ToastProvider>
        </LockGate>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}

/** console.error calls that mention `what`. */
const reported = (errors: ReturnType<typeof spyOn>, what: string) =>
  errors.mock.calls.filter((call: unknown[]) => call.some((part) => String(part).includes(what)));

function OpenWizard() {
  const show = useOverlay();
  useEffect(() => {
    if (opened) return;
    opened = true;
    show((close) => <AddTargetWizard onClose={close} />);
  }, [show]);
  return null;
}

const changed = (isLocked: boolean) =>
  act(async () => {
    locked = isLocked;
    await emit(
      LOCK_CHANGED,
      lockState({ locked: isLocked, reason: isLocked ? 'manual' : null, seq: isLocked ? 1 : 2 }),
    );
    await new Promise((r) => setTimeout(r, 0));
  });

test('a lock unmounts the wizard with its token; the verify it ran is cancelled; unlock does not bring it back', async () => {
  const errors = spyOn(console, 'error');
  const user = renderShell();
  await user.type(await screen.findByLabelText('API token'), TOKEN);
  await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
  expect(screen.getByRole('button', { name: 'Verifying…' })).toBeDefined();

  await changed(true);
  expect(screen.queryByRole('dialog', { name: 'Add target' })).toBeNull();
  expect(document.body.innerHTML).not.toContain(TOKEN);
  expect(calls).toContain('op_cancel');
  expect(reported(errors, 'op_cancel')).toEqual([]);

  await changed(false);
  expect(screen.queryByRole('dialog', { name: 'Add target' })).toBeNull();
  errors.mockRestore();
});

test('a lock after the verify drops the draft and cancels the catalogue read; both refusals are quiet', async () => {
  verifyEnds = true;
  const errors = spyOn(console, 'error');
  const user = renderShell();
  await user.type(await screen.findByLabelText('API token'), TOKEN);
  await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
  expect(await screen.findByText("Reading the provider's catalogue…")).toBeDefined();
  expect(calls).toContain('op_start_machine_catalogue');

  await changed(true);
  expect(screen.queryByRole('dialog', { name: 'Add target' })).toBeNull();
  expect(calls).toContain('target_draft_discard');
  expect(calls).toContain('op_cancel');
  expect(reported(errors, 'target_draft_discard')).toEqual([]);
  expect(reported(errors, 'op_cancel')).toEqual([]);

  await changed(false);
  expect(screen.queryByRole('dialog', { name: 'Add target' })).toBeNull();
  errors.mockRestore();
});
