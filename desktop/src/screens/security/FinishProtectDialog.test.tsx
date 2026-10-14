// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, screen, waitFor } from '@testing-library/react';
import { CORE_ERROR_CODES } from '../../ipc/generated/core-errors';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import type { PlanView } from '../../ipc/generated/PlanView';
import { resetHeldPlans } from '../../ipc/heldPlans';
import { resetOperations } from '../../ipc/operations';
import { appInfo } from '../../test/fixtures';
import { completed, type Harness, installHarness, uiError } from '../../test/ipc';
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
