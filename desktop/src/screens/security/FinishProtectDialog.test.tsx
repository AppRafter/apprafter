// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, within } from '@testing-library/react';
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
import { settleIpc, until } from '../../test/settle';
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

/**
 * The plan's answer, which the test gives, inside `act`. Every step here gives what it drives,
 * then waits for what must follow as an event (`until`), never for a time (GOTCHA-332: a
 * `waitFor` timed out on a loaded machine).
 */
function held<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return {
    promise,
    answer: (value: T) =>
      act(async () => {
        resolve(value);
      }),
    refuse: (reason: unknown) =>
      act(async () => {
        reject(reason);
      }),
  };
}

/** The held run, asked for, ends now. */
async function ended(run: { end(): void }) {
  await until('the run asked for', () => h.of('op_execute').length === 1);
  await act(async () => {
    run.end();
    await settleIpc();
  });
}

/** The confirm, once the plan's answer has brought it. */
async function confirmShown() {
  await until('the confirm', () => screen.queryByRole('button', { name: 'Finish' }) !== null);
  return screen.getByRole('button', { name: 'Finish' });
}

/** The text, once it shows. */
async function shown(text: string) {
  await until(text, () => screen.queryByText(text) !== null);
  return screen.getByText(text);
}

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
  const planned = held<PlanView>();
  const plan = mock((_password: string, _recovery: string) => planned.promise);
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
  await until('the plan asked for', () => plan.mock.calls.length > 0);
  expect(plan).toHaveBeenCalledWith('the lock password', 'QQQSY-QCYQ5');
  await planned.answer(view);
  const run = h.holdRuns();
  await user.click(await confirmShown());
  expect(onDone).not.toHaveBeenCalled();
  await ended(run);
  await until('the finish done', () => onDone.mock.calls.length > 0);
  expect(h.of('op_execute').map((c) => c.args.opId)).toEqual([opId]);
  // What the run reported, said (UI verifier M3): a token left in clear is named.
  await until('its toast', () =>
    Boolean(screen.queryByText(/^Secrets protected\. The credentials file of prod holds a token/)),
  );
});

test("a secret no slot takes says so, in the store's words", async () => {
  const planned = held<PlanView>();
  const plan = mock(() => planned.promise);
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
  await planned.refuse(
    uiError(DESKTOP_ERROR_CODES.AUTH_FAILED, 'authentication failed', { exhausted: false }),
  );
  await shown('That is not the lock password or the recovery key.');
  // T78: neither secret stays in the page after the answer (critic B finding 17).
  expect((screen.getByLabelText('Lock password') as HTMLInputElement).value).toBe('');
  expect((screen.getByLabelText('Recovery key') as HTMLInputElement).value).toBe('');
});

test('the back-off is counted down and holds Continue, as on the lock screen (UI verifier minor 3)', async () => {
  const planned = held<PlanView>();
  const plan = mock(() => planned.promise);
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
  await planned.refuse(
    uiError(DESKTOP_ERROR_CODES.AUTH_FAILED, 'x', { exhausted: true, retryInMs: 30_000 }),
  );
  await shown('Too many failed attempts. Try again in 30 s.');
  // Nothing was checked: no line says a secret was wrong.
  expect(screen.queryByText('x')).toBeNull();
  expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(
    true,
  );
});

test('a mistyped recovery key is said where it is wrong', async () => {
  const planned = held<PlanView>();
  const plan = mock(() => planned.promise);
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
  await planned.refuse(
    uiError(CORE_ERROR_CODES.SECRETS_RECOVERY_KEY_INVALID, 'x', { reason: 'checksum', group: 7 }),
  );
  await shown('That key is mistyped near group 7.');
});

// WI-484 (WI-483's concern 2; the controller's decision): Finish's failure was a 3.4 s toast, its
// help unsaid, while Stop protecting says its failure in an ErrorPanel in its place. Finish says
// it as Stop protecting does now: the message, the help and the code, in the flow's own dialog,
// until it is closed; and a lock while it shows keeps it for after the unlock, under the plan's
// title (the lock is the session's end, as LockGate ends it). The toasts' clock never runs here.
const VAULT_DAMAGED = UI_ERRORS.vaultUnreadable as UiError;
const VAULT_DAMAGED_CODE: string = UI_ERRORS.vaultUnreadable.code;

/**
 * Finish with both secrets, then its confirm, on a plan whose run ends in `error`: once this
 * returns, React has applied that end.
 */
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
  const planned = held<PlanView>();
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <ToastProvider schedule={() => () => {}}>
          <FinishProtectDialog
            store="~/.config/apprafter"
            auth={appInfo().auth}
            plan={() => planned.promise}
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
  await planned.answer(view);
  const run = h.holdRuns();
  await user.click(await confirmShown());
  await ended(run);
  await until('the failure in the dialog', () => {
    const dialog = screen.queryByRole('dialog', { name: FINISH_TITLE });
    return dialog !== null && within(dialog).queryByRole('alert') !== null;
  });
  return { user, onDone };
}

test('WI-484: a finish that fails says so in its dialog: the message, the help and the code', async () => {
  const onClose = mock();
  const { user, onDone } = await finishFails(VAULT_DAMAGED, onClose);
  const dialog = screen.getByRole('dialog', { name: FINISH_TITLE });
  const panel = within(dialog).getByRole('alert');
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
    within(screen.getByRole('dialog', { name: FINISH_TITLE })).getByRole('alert'),
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
