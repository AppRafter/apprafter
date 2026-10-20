// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import { endedAwaySnapshot, resetEndedAway } from '../../ipc/away';
import { CORE_ERROR_CODES } from '../../ipc/generated/core-errors';
import type { LockOnPlan } from '../../ipc/generated/LockOnPlan';
import type { OpId } from '../../ipc/generated/OpId';
import { resetHeldPlans } from '../../ipc/heldPlans';
import { resetLifecycle, sessionLocked } from '../../ipc/lifecycle';
import { resetOperations } from '../../ipc/operations';
import { PROTECT_TITLE } from '../../state/lockText';
import { completed, failed, type Harness, installHarness, stage, uiError } from '../../test/ipc';
import { renderScreen } from '../../test/screens';
import { settleIpc } from '../../test/settle';
import { cancellableAt, ProtectDialog, type ProtectDialogProps } from './ProtectDialog';

/** T24's first vector (bytes 00..1f), as `lock on` shows it: a published test value, nobody's key. */
const KEY =
  'APPRAFTER-RECOVERY-KEY-1\nQQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK';
/**
 * The key as a person types it back into the one-line password field: a password input drops a
 * newline (happy-dom's sanitizer, as a browser's), so its line break is a space — which Rust's
 * parse ignores, as it ignores dashes (UI verifier M1).
 */
const TYPED_BACK = KEY.replace('\n', ' ');

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

function planOf(opId: OpId): LockOnPlan {
  return {
    plan: {
      opId,
      class: 'bounded',
      title: 'Lock the secrets of the target store at ~/.config/apprafter',
      changes: [
        { kind: 'Credentials', object: 'prod', action: 'move', detail: 'credentials.yaml → vault' },
      ],
      target: null,
      expiresAtMs: Date.now() + 600_000,
      gesture: 'none',
    },
    recoveryKey: KEY,
  };
}

function open(
  more: Partial<ProtectDialogProps> = {},
  end: ReturnType<typeof completed | typeof failed> = completed({ alreadyLocked: false }),
) {
  const opId = h.newOperation([
    stage(1, 4, 'Writing the vault'),
    stage(2, 4, 'Checking the vault'),
    stage(3, 4, 'Re-encrypting cached secrets'),
    stage(4, 4, 'Removing the secrets from files'),
    end,
  ]);
  /** The order Rust is asked in: "I saved it" must come before the key is checked (O12). */
  const order: string[] = [];
  // The calls, kept apart from `more` so each keeps its mock's type (exactOptionalPropertyTypes:
  // a spread of a Partial would widen each to the prop's plain function type).
  const props = {
    plan: mock((_password: string) => Promise.resolve(planOf(opId))),
    keySaved: mock((_opId: OpId) => {
      order.push('saved');
      return Promise.resolve();
    }),
    confirmKey: mock((_opId: OpId, _typed: string) => {
      order.push('confirm');
      return Promise.resolve();
    }),
    copyKey: mock((_opId: OpId) => Promise.resolve()),
    onDone: mock(),
    onClose: mock(),
  };
  const user = renderScreen(
    <ProtectDialog store="~/.config/apprafter" restart={false} {...props} {...more} />,
  );
  return { user, props, opId, order };
}

const CONFIRM = 'Type the whole recovery key to confirm you saved it';

const next = (user: ReturnType<typeof open>['user'], name = 'Continue') =>
  user.click(screen.getByRole('button', { name }));

/** Every step to the run: a long password twice, "I saved it", the key typed back, Protect. */
async function protect(user: ReturnType<typeof open>['user']) {
  await next(user);
  await user.type(screen.getByLabelText('Lock password'), 'a long lock password here');
  await user.type(screen.getByLabelText('Repeat the lock password'), 'a long lock password here');
  await next(user);
  await waitFor(() => expect(screen.getByLabelText('Recovery key')).toBeDefined());
  await next(user, 'I saved it');
  await user.type(screen.getByLabelText(CONFIRM), TYPED_BACK);
  await next(user, 'Protect');
}

describe('Protect secrets', () => {
  test('step 1 says what it does, what it stops and what it does not', async () => {
    open({ restart: true });
    expect(
      screen.getByText(/Encrypts the secrets of the target store at ~\/.config\/apprafter/),
    ).toBeDefined();
    expect(
      screen.getByText(/run agents under a separate OS account or in a sandbox/),
    ).toBeDefined();
    expect(screen.getByText(/The lock protects secrets at rest\./)).toBeDefined();
    expect(screen.getByText(/The recovery key shown then is not valid/)).toBeDefined();
  });

  test('fourteen characters are warned about and still go on, sent to Rust once (O2)', async () => {
    const { user, props } = open();
    await next(user);
    await user.type(screen.getByLabelText('Lock password'), 'fourteen-chars');
    expect(screen.getByText(/Shorter than 15 characters\./)).toBeDefined();
    await user.type(screen.getByLabelText('Repeat the lock password'), 'fourteen-chars');
    await next(user);
    await waitFor(() => expect(props.plan).toHaveBeenCalledWith('fourteen-chars'));
    expect(props.plan).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText('Recovery key').textContent).toBe(KEY);
  });

  test('an empty password is refused here, two that differ too, and a key-shaped one by Rust', async () => {
    const { user, props } = open();
    await next(user);
    await next(user);
    expect(screen.getByRole('alert').textContent).toBe('Type a lock password.');
    await user.type(screen.getByLabelText('Lock password'), 'one password');
    await user.type(screen.getByLabelText('Repeat the lock password'), 'another');
    await next(user);
    expect(screen.getByRole('alert').textContent).toBe('The two passwords differ.');
    expect(props.plan).not.toHaveBeenCalled();
    props.plan.mockImplementation(() =>
      Promise.reject(
        uiError(CORE_ERROR_CODES.SECRETS_PASSWORD_REFUSED, 'refused', {
          reason: 'recovery_key_shape',
        }),
      ),
    );
    await user.clear(screen.getByLabelText('Repeat the lock password'));
    await user.type(screen.getByLabelText('Repeat the lock password'), 'one password');
    await next(user);
    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toBe(
        'That has the shape of a recovery key. Choose another.',
      ),
    );
  });

  test('the key: shown once, copied by Rust and never from the page, taken off the clipboard when saved, typed back hidden', async () => {
    const { user, props, opId } = open();
    await next(user);
    await user.type(screen.getByLabelText('Lock password'), 'a long lock password here');
    await user.type(screen.getByLabelText('Repeat the lock password'), 'a long lock password here');
    await next(user);
    await waitFor(() => expect(screen.getByLabelText('Recovery key')).toBeDefined());
    // The web view offers no copy of its own (ADR 0068 §3): the copy event is refused.
    expect(fireEvent.copy(screen.getByLabelText('Recovery key'))).toBe(false);
    await user.click(screen.getByRole('button', { name: 'Copy' }));
    expect(props.copyKey).toHaveBeenCalledWith(opId);
    await waitFor(() =>
      expect(screen.getByText('Copied. The clipboard is cleared in 60 s.')).toBeDefined(),
    );
    expect(screen.getByText(/include ~\/\.config\/apprafter\/vault in your backups/)).toBeDefined();
    await next(user, 'I saved it');
    // Rust took its copy off the clipboard: what is typed next comes from where it was saved.
    expect(props.keySaved).toHaveBeenCalledWith(opId);
    expect(screen.queryByLabelText('Recovery key')).toBeNull();
    expect(document.body.textContent).not.toContain('QQQSY');
    const field = screen.getByLabelText(CONFIRM) as HTMLInputElement;
    expect(field.type).toBe('password');
    props.confirmKey.mockImplementationOnce(() =>
      Promise.reject(
        uiError(CORE_ERROR_CODES.SECRETS_RECOVERY_KEY_INVALID, 'x', { reason: 'mismatch' }),
      ),
    );
    await user.type(field, 'not it');
    await next(user, 'Protect');
    await waitFor(() =>
      expect(screen.getByRole('alert').textContent).toBe('That is not the key shown.'),
    );
    // The typed key left the page's state before it was checked (critic A finding 7).
    expect((screen.getByLabelText(CONFIRM) as HTMLInputElement).value).toBe('');
    await user.click(screen.getByRole('button', { name: 'Show it again' }));
    expect(screen.getByLabelText('Recovery key').textContent).toBe(KEY);
    // Once saved, the key is not copied again (O12).
    expect(screen.queryByRole('button', { name: 'Copy' })).toBeNull();
    expect(screen.getByText(/Taken off the clipboard when you chose I saved it/)).toBeDefined();
  });

  test('typed back, it runs: the toast, onDone, and the plan never discarded before it ran', async () => {
    const { user, props, opId, order } = open();
    await protect(user);
    await waitFor(() => expect(props.onDone).toHaveBeenCalled());
    expect(props.confirmKey).toHaveBeenCalledWith(opId, TYPED_BACK);
    expect(order).toEqual(['saved', 'confirm']);
    expect(h.of('op_execute').map((c) => c.args.opId)).toEqual([opId]);
    expect(h.of('op_execute')[0]?.args.password).toBeUndefined();
    expect(
      screen.getByText('Secrets protected. Unlocking AppRafter now opens them.'),
    ).toBeDefined();
    // Never discarded before it ran (startPlan discards it once its end is shown: ipc/plans.ts).
    const ran = h.calls.findIndex((c) => c.cmd === 'op_execute');
    const before = h.calls.slice(0, ran);
    expect(before.filter((c) => c.cmd === 'op_discard' && c.args.opId === opId)).toEqual([]);
  });

  test('a run another one beat is said as it happened, never "protected" (UI verifier M3)', async () => {
    const { user, props } = open({}, completed({ alreadyLocked: true }));
    await protect(user);
    await waitFor(() => expect(props.onDone).toHaveBeenCalled());
    expect(screen.getByText(/^Another run protected this target store first/)).toBeDefined();
    expect(screen.queryByText(/^Secrets protected/)).toBeNull();
  });

  // WI-483 (from N5): the run's failure is said in the dialog, in an ErrorPanel, and a lock
  // unmounts it with the shell. Still shown when the app locks, it is kept for after the unlock,
  // under the run's title, with its code.
  test('WI-483: a run that fails, still shown when the app locks, is kept with its code', async () => {
    const code = 'apprafter::secrets::vault_unwritable';
    const error = uiError(
      code,
      'cannot write the vault at ~/.config/apprafter/vault: no space left on device',
    );
    const { user } = open({}, failed(error));
    await protect(user);
    expect(await screen.findByText(error.message)).toBeDefined();
    expect(endedAwaySnapshot()).toEqual([]);
    act(() => {
      sessionLocked();
    });
    expect(endedAwaySnapshot()).toEqual([
      {
        opId: null,
        text: `${PROTECT_TITLE} failed: ${error.message}`,
        failed: true,
        code,
      },
    ]);
  });

  test('WI-483: a refusal before the run, shown when the app locks, is not kept', async () => {
    const refusal = uiError(
      'apprafter::secrets::store_not_private',
      'other accounts can change the target store at ~/.config/apprafter',
    );
    const { user } = open({ plan: mock((_password: string) => Promise.reject(refusal)) });
    await next(user);
    await user.type(screen.getByLabelText('Lock password'), 'a long lock password here');
    await user.type(screen.getByLabelText('Repeat the lock password'), 'a long lock password here');
    await next(user);
    expect(await screen.findByText(refusal.message)).toBeDefined();
    act(() => {
      sessionLocked();
    });
    expect(endedAwaySnapshot()).toEqual([]);
  });

  test('WI-483: a run refused before it started, shown when the app locks, is not kept', async () => {
    const refusal = uiError(
      'apprafter::secrets::store_not_private',
      'other accounts can change the target store at ~/.config/apprafter',
    );
    h.answer('op_execute', () => Promise.reject(refusal));
    const { user } = open();
    await protect(user);
    expect(await screen.findByText(refusal.message)).toBeDefined();
    act(() => {
      sessionLocked();
    });
    expect(endedAwaySnapshot()).toEqual([]);
  });

  test('the dialog has no clipboard of its own: its Copy is the call it is given (O12)', async () => {
    // A source check (the critic's vacuous assertion replaced: UI verifier minor 7): neither
    // the clipboard plugin, the app's copy hook nor the web view's clipboard is reached here.
    const src = await Bun.file(new URL('./ProtectDialog.tsx', import.meta.url)).text();
    expect(src).not.toMatch(/plugin-clipboard-manager|state\/copy|navigator\.clipboard|writeText/);
  });

  test('Back past the key, or Close before the run, discards the plan in Rust', async () => {
    const { user, props, opId } = open();
    await next(user);
    await user.type(screen.getByLabelText('Lock password'), 'a long lock password here');
    await user.type(screen.getByLabelText('Repeat the lock password'), 'a long lock password here');
    await next(user);
    await waitFor(() => expect(screen.getByLabelText('Recovery key')).toBeDefined());
    await user.click(screen.getByRole('button', { name: 'Back' }));
    await waitFor(() => expect(h.of('op_discard').map((c) => c.args.opId)).toEqual([opId]));
    expect((screen.getByLabelText('Lock password') as HTMLInputElement).value).toBe('');
    await user.click(screen.getByRole('button', { name: 'Close' }));
    expect(props.onClose).toHaveBeenCalled();
  });

  test('Cancel stays until the run starts re-encrypting caches (§3.9)', () => {
    expect(cancellableAt(null)).toBe(true);
    expect(cancellableAt({ index: 1, total: 4, title: 'Writing the vault' })).toBe(true);
    expect(cancellableAt({ index: 2, total: 4, title: 'Checking the vault' })).toBe(true);
    expect(cancellableAt({ index: 3, total: 4, title: 'Re-encrypting cached secrets' })).toBe(
      false,
    );
    expect(cancellableAt({ index: 4, total: 4, title: 'Removing the secrets from files' })).toBe(
      false,
    );
  });
});
