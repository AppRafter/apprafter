// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import { endedAwaySnapshot, resetEndedAway } from '../../ipc/away';
import { CORE_ERROR_CODES } from '../../ipc/generated/core-errors';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import UI_ERRORS from '../../ipc/generated/fixtures/ui-errors.json';
import type { PlanView } from '../../ipc/generated/PlanView';
import type { UiError } from '../../ipc/generated/UiError';
import { resetHeldPlans } from '../../ipc/heldPlans';
import { resetLifecycle, sessionLocked } from '../../ipc/lifecycle';
import { resetOperations } from '../../ipc/operations';
import { FINISH_TITLE } from '../../state/lockText';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo } from '../../test/fixtures';
import { completed, failed, type Harness, installHarness, uiError } from '../../test/ipc';
import { renderScreen } from '../../test/screens';
import { settleIpc } from '../../test/settle';
import { FinishProtectDialog } from './FinishProtectDialog';

let h: Harness;
beforeEach(() => {
  h = installHarness();
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  resetHeldPlans();
  resetEndedAway();
  resetLifecycle();
  clearMocks();
});

test('both secrets, then a plain confirm that runs the finish (team decision 2026-10-13)', async () => {
  const opId = h.newOperation([completed({ alreadyLocked: false, tokensLeft: ['prod'] })]);
  const view: PlanView = {
    opId,
    class: 'bounded',
    title: 'Finish locking',
    changes: [],
    target: null,
    expiresAtMs: Date.now() + 600_000,
    gesture: 'none',
  };
  const plan = mock((_password: string, _recovery: string) => Promise.resolve(view));
  const onDone = mock();
  const user = renderScreen(
    <FinishProtectDialog
      store="~/.config/apprafter"
      auth={appInfo().auth}
      plan={plan}
      onDone={onDone}
      onClose={mock()}
    />,
  );
  expect(screen.getByText(/both the lock password and the recovery key/)).toBeDefined();
  await user.type(screen.getByLabelText('Lock password'), 'the lock password');
  await user.type(screen.getByLabelText('Recovery key'), 'QQQSY-QCYQ5');
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await waitFor(() => expect(plan).toHaveBeenCalledWith('the lock password', 'QQQSY-QCYQ5'));
  await user.click(await screen.findByRole('button', { name: 'Finish' }));
  await waitFor(() => expect(onDone).toHaveBeenCalled());
  expect(h.of('op_execute').map((c) => c.args.opId)).toEqual([opId]);
  // What the run reported, said (UI verifier M3): a token left in clear is named.
  expect(
    screen.getByText(/^Secrets protected\. The credentials file of prod holds a token/),
  ).toBeDefined();
});

test("a secret no slot takes says so, in the store's words", async () => {
  const plan = mock(() =>
    Promise.reject(
      uiError(DESKTOP_ERROR_CODES.AUTH_FAILED, 'authentication failed', { exhausted: false }),
    ),
  );
  const user = renderScreen(
    <FinishProtectDialog
      store="~/s"
      auth={appInfo().auth}
      plan={plan}
      onDone={mock()}
      onClose={mock()}
    />,
  );
  await user.type(screen.getByLabelText('Lock password'), 'wrong');
  await user.type(screen.getByLabelText('Recovery key'), 'wrong');
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await waitFor(() =>
    expect(screen.getByText('That is not the lock password or the recovery key.')).toBeDefined(),
  );
  // T78: neither secret stays in the page after the answer (critic B finding 17).
  expect((screen.getByLabelText('Lock password') as HTMLInputElement).value).toBe('');
  expect((screen.getByLabelText('Recovery key') as HTMLInputElement).value).toBe('');
});

test('the back-off is counted down and holds Continue, as on the lock screen (UI verifier minor 3)', async () => {
  const plan = mock(() =>
    Promise.reject(
      uiError(DESKTOP_ERROR_CODES.AUTH_FAILED, 'x', { exhausted: true, retryInMs: 30_000 }),
    ),
  );
  const user = renderScreen(
    <FinishProtectDialog
      store="~/s"
      auth={appInfo().auth}
      plan={plan}
      onDone={mock()}
      onClose={mock()}
    />,
  );
  await user.type(screen.getByLabelText('Lock password'), 'wrong');
  await user.type(screen.getByLabelText('Recovery key'), 'wrong');
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await waitFor(() =>
    expect(screen.getByText('Too many failed attempts. Try again in 30 s.')).toBeDefined(),
  );
  // Nothing was checked: no line says a secret was wrong.
  expect(screen.queryByText('x')).toBeNull();
  expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(
    true,
  );
});

test('a mistyped recovery key is said where it is wrong', async () => {
  const plan = mock(() =>
    Promise.reject(
      uiError(CORE_ERROR_CODES.SECRETS_RECOVERY_KEY_INVALID, 'x', { reason: 'checksum', group: 7 }),
    ),
  );
  const user = renderScreen(
    <FinishProtectDialog
      store="~/s"
      auth={appInfo().auth}
      plan={plan}
      onDone={mock()}
      onClose={mock()}
    />,
  );
  await user.type(screen.getByLabelText('Lock password'), 'the lock password');
  await user.type(screen.getByLabelText('Recovery key'), 'QQQSY-QCYQ6');
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await waitFor(() => expect(screen.getByText('That key is mistyped near group 7.')).toBeDefined());
});

// WI-484 (WI-483's concern 2; the controller's decision): Finish's failure was a 3.4 s toast, its
// help unsaid, while Stop protecting says its failure in an ErrorPanel in its place. Finish says
// it as Stop protecting does now: the message, the help and the code, in the flow's own dialog,
// until it is closed; and a lock while it shows keeps it for after the unlock, under the plan's
// title (the lock is the session's end, as LockGate ends it). The toasts' clock never runs here.
const VAULT_DAMAGED = UI_ERRORS.vaultUnreadable as UiError;
const VAULT_DAMAGED_CODE: string = UI_ERRORS.vaultUnreadable.code;

/** Finish with both secrets, then its confirm, on a plan whose run ends in `error`. */
async function finishFails(error: UiError, onClose: () => void = mock()) {
  const opId = h.newOperation([failed(error)]);
  const view: PlanView = {
    opId,
    class: 'bounded',
    title: 'Finish locking',
    changes: [],
    target: null,
    expiresAtMs: Date.now() + 600_000,
    gesture: 'none',
  };
  const onDone = mock();
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <ToastProvider schedule={() => () => {}}>
          <FinishProtectDialog
            store="~/.config/apprafter"
            auth={appInfo().auth}
            plan={() => Promise.resolve(view)}
            onDone={onDone}
            onClose={onClose}
          />
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  const user = userEvent.setup();
  await user.type(screen.getByLabelText('Lock password'), 'the lock password');
  await user.type(screen.getByLabelText('Recovery key'), 'QQQSY-QCYQ5');
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await user.click(await screen.findByRole('button', { name: 'Finish' }));
  return { user, onDone };
}

test('WI-484: a finish that fails says so in its dialog: the message, the help and the code', async () => {
  const onClose = mock();
  const { user, onDone } = await finishFails(VAULT_DAMAGED, onClose);
  const dialog = await screen.findByRole('dialog', { name: FINISH_TITLE });
  const panel = await within(dialog).findByRole('alert');
  expect(panel.textContent).toContain(VAULT_DAMAGED.message);
  expect(panel.textContent).toContain(VAULT_DAMAGED.help ?? '?');
  expect(panel.textContent).toContain(VAULT_DAMAGED_CODE);
  // Said once, in its place: no toast, and the dialog stays until it is closed.
  expect(screen.getByRole('status').textContent).toBe('');
  expect(onClose).not.toHaveBeenCalled();
  expect(onDone).not.toHaveBeenCalled();
  await user.click(within(dialog).getByRole('button', { name: 'Close' }));
  expect(onClose).toHaveBeenCalledTimes(1);
});

test('WI-484: a finish’s failure, still shown when the app locks, is kept with its code', async () => {
  await finishFails(VAULT_DAMAGED);
  expect(
    await within(await screen.findByRole('dialog', { name: FINISH_TITLE })).findByRole('alert'),
  ).toBeDefined();
  expect(endedAwaySnapshot()).toEqual([]);
  act(() => {
    sessionLocked();
  });
  expect(endedAwaySnapshot()).toEqual([
    {
      opId: null,
      text: `Finish locking failed: ${VAULT_DAMAGED.message}`,
      failed: true,
      code: VAULT_DAMAGED_CODE,
    },
  ]);
});
