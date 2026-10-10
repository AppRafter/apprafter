// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, jest, mock, test } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup as cleanupDialog, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { JsonValue } from '../ipc/generated/serde_json/JsonValue';
import { execute, resetOperations } from '../ipc/operations';
import { ConfirmDialog, type ConfirmDialogProps, needsDialog } from './ConfirmDialog';

const hello: AuthInfo = {
  available: true,
  method: 'windows_hello',
  unavailable: null,
  biometricsChoice: true,
  passwordField: false,
};

/** Linux where polkit cannot prompt: the PAM route, and the dialog's own field. */
const pam: AuthInfo = { ...hello, method: 'pam', biometricsChoice: false, passwordField: true };

const refusal = (code: string, fields: Record<string, JsonValue> = {}) => ({
  code,
  message: `Rust says ${code}`,
  help: null,
  causes: [],
  fields,
});

function open(props: Partial<ConfirmDialogProps> = {}) {
  const onConfirm = mock((_password?: string) => Promise.resolve());
  const onClose = mock();
  render(
    <ConfirmDialog
      title="Remove target prod-eu"
      body="The target store forgets it. The cluster keeps running."
      confirmLabel="Remove"
      planClass="destructive"
      auth={hello}
      onConfirm={onConfirm}
      onClose={onClose}
      {...props}
    />,
  );
  // Under fake timers user-event must not pause between keys: nothing would end the pause.
  return { user: userEvent.setup(fakeTime ? { delay: null } : {}), onConfirm, onClose };
}

/** Set while a test runs on fake timers (onFakeTime). */
let fakeTime = false;

/** Run `body` with the clock faked (setTimeout and performance.now alike). */
async function onFakeTime(body: () => Promise<void>) {
  jest.useFakeTimers();
  fakeTime = true;
  try {
    await body();
  } finally {
    fakeTime = false;
    jest.useRealTimers();
  }
}

/**
 * What a browser does to a control that is disabled while it has the focus: it loses it.
 * happy-dom keeps it there and ignores blur() on a disabled control, so the tests move the focus
 * to a throwaway input and remove it, which leaves it on the body.
 */
const loseFocus = () =>
  act(() => {
    const elsewhere = document.createElement('input');
    document.body.append(elsewhere);
    elsewhere.focus();
    elsewhere.remove();
    expect(document.activeElement).toBe(document.body);
  });

/** Let a refused onConfirm reach the dialog: its promise chain settles in microtasks. */
const settled = () =>
  act(async () => {
    for (let i = 0; i < 20; i += 1) await Promise.resolve();
  });

const confirm = () => screen.getByRole('button', { name: 'Remove' }) as HTMLButtonElement;

describe('needsDialog', () => {
  test('a reversible plan runs without one; bounded and destructive ask', () => {
    expect(needsDialog('reversible')).toBe(false);
    expect(needsDialog('bounded')).toBe(true);
    expect(needsDialog('destructive')).toBe(true);
  });
});

describe('ConfirmDialog', () => {
  test('bounded is a plain confirm: no plan, no typing, no OS prompt named', async () => {
    const { user, onConfirm, onClose } = open({
      planClass: 'bounded',
      plan: <p>the plan</p>,
      requireText: 'prod-eu',
    });
    expect(screen.queryByText('the plan')).toBeNull();
    expect(screen.queryByRole('textbox')).toBeNull();
    expect(screen.queryByText(/next\.$/)).toBeNull();
    await user.click(confirm());
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test('destructive shows the plan and names the OS prompt that follows', () => {
    open({ plan: <p>delete Target prod-eu</p> });
    expect(screen.getByText('delete Target prod-eu')).toBeDefined();
    expect(screen.getByText('Confirm with Windows Hello next.')).toBeDefined();
  });

  test('the typed name must match exactly, case and spaces included', async () => {
    const { user, onConfirm } = open({ requireText: 'prod-eu' });
    const field = screen.getByLabelText('Type prod-eu to confirm');
    expect(confirm().disabled).toBe(true);
    await user.type(field, 'PROD-EU');
    expect(confirm().disabled).toBe(true);
    await user.clear(field);
    await user.type(field, 'prod-eu ');
    expect(confirm().disabled).toBe(true);
    await user.clear(field);
    await user.type(field, 'prod-eu');
    expect(confirm().disabled).toBe(false);
    await user.click(confirm());
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  test('the prompt is named per OS method, and not at all when unknown', () => {
    open({ auth: { ...hello, method: 'mac_local_authentication' } });
    expect(screen.getByText('Confirm with Touch ID or your Mac password next.')).toBeDefined();
  });

  test('without a method nothing is promised', () => {
    open({ auth: { ...hello, available: false, method: null, unavailable: 'no_backend' } });
    expect(screen.queryByText(/next\.$/)).toBeNull();
  });

  test('an approve that is not destructive can still say a prompt follows', () => {
    open({ planClass: 'bounded', osGesture: true, auth: { ...hello, method: 'polkit' } });
    expect(screen.getByText('Confirm with your account password next.')).toBeDefined();
  });

  test('danger makes the confirm button the solid danger one', () => {
    open({ danger: true });
    expect(confirm().dataset.variant).toBe('danger-solid');
  });

  test('a refusal is shown inline and the dialog stays', async () => {
    const refused = {
      code: 'apprafter::desktop::auth_cancelled',
      message: 'The confirmation was cancelled.',
      help: null,
      causes: [],
      fields: {},
    };
    const { user, onClose } = open({
      planClass: 'bounded',
      onConfirm: () => Promise.reject(refused),
    });
    await user.click(confirm());
    expect(screen.getByRole('alert').textContent).toContain('The confirmation was cancelled.');
    expect(onClose).not.toHaveBeenCalled();
  });
});

describe('ConfirmDialog where the OS cannot prompt: the password field', () => {
  const password = () => screen.getByLabelText('System password') as HTMLInputElement;

  test('a destructive plan asks for the password here, and hands it to onConfirm', async () => {
    const { user, onConfirm, onClose } = open({ auth: pam });
    expect(screen.queryByText(/next\.$/)).toBeNull();
    expect(confirm().disabled).toBe(true);
    await user.type(password(), 'hunter2');
    await user.click(confirm());
    expect(onConfirm).toHaveBeenCalledWith('hunter2');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test('Enter in the field confirms', async () => {
    const { user, onConfirm } = open({ auth: pam });
    await user.type(password(), 'hunter2{Enter}');
    expect(onConfirm).toHaveBeenCalledWith('hunter2');
  });

  test('the password goes only with the field: an OS prompt, or no gesture, gets none', async () => {
    const prompted = open();
    await prompted.user.click(confirm());
    expect(prompted.onConfirm).toHaveBeenCalledTimes(1);
    expect(prompted.onConfirm.mock.calls[0]).toEqual([]);
    cleanupDialog();
    const bounded = open({ planClass: 'bounded', auth: pam });
    expect(screen.queryByLabelText('System password')).toBeNull();
    await bounded.user.click(confirm());
    expect(bounded.onConfirm.mock.calls[0]).toEqual([]);
  });

  test('with the name to type as well, both are needed', async () => {
    const { user, onConfirm } = open({ auth: pam, requireText: 'prod-eu' });
    await user.type(screen.getByLabelText('Type prod-eu to confirm'), 'prod-eu');
    expect(confirm().disabled).toBe(true);
    await user.type(password(), 'hunter2');
    await user.click(confirm());
    expect(onConfirm).toHaveBeenCalledWith('hunter2');
  });

  test("a wrong password: the OS's words, the field emptied, and the dialog stays", async () => {
    const { user, onClose } = open({
      auth: pam,
      onConfirm: () =>
        Promise.reject(
          refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, {
            exhausted: false,
            messages: ['Authentication failure'],
          }),
        ),
    });
    await user.type(password(), 'guess{Enter}');
    expect((await screen.findByRole('alert')).textContent).toBe('Authentication failure');
    expect(password().value).toBe('');
    expect(onClose).not.toHaveBeenCalled();
  });

  test('a refusal marks the field until the owner types again, as the lock screen does', async () => {
    const { user } = open({
      auth: pam,
      onConfirm: () =>
        Promise.reject(refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: false })),
    });
    expect(password().getAttribute('aria-invalid')).toBeNull();
    await user.type(password(), 'guess{Enter}');
    await screen.findByRole('alert');
    expect(password().getAttribute('aria-invalid')).toBe('true');
    await user.type(password(), 'h');
    expect(password().getAttribute('aria-invalid')).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  test('…and that the password is not right when the OS said nothing', async () => {
    const { user } = open({
      auth: pam,
      onConfirm: () => Promise.reject(refusal(DESKTOP_ERROR_CODES.AUTH_FAILED)),
    });
    await user.type(password(), 'guess{Enter}');
    expect((await screen.findByRole('alert')).textContent).toBe('That password is not right.');
  });

  test("too many failed attempts: field and confirm count down Rust's retryInMs, then refocus", () =>
    onFakeTime(async () => {
      let answer = (): Promise<void> =>
        Promise.reject(
          refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true, retryInMs: 1_500 }),
        );
      const { user, onClose } = open({ auth: pam, onConfirm: () => answer() });
      await user.type(password(), 'guess{Enter}');
      await settled();
      expect(screen.getByRole('alert').textContent).toBe(
        'Too many failed attempts. Try again in 2 s.',
      );
      expect(password().disabled).toBe(true);
      expect(confirm().disabled).toBe(true);
      loseFocus();
      act(() => jest.advanceTimersByTime(500));
      expect(screen.getByRole('alert').textContent).toBe(
        'Too many failed attempts. Try again in 1 s.',
      );
      act(() => jest.advanceTimersByTime(1_000));
      expect(screen.queryByRole('alert')).toBeNull();
      expect(password().disabled).toBe(false);
      expect(document.activeElement).toBe(password());
      answer = () => Promise.resolve();
      await user.type(password(), 'hunter2{Enter}');
      await settled();
      expect(onClose).toHaveBeenCalledTimes(1);
    }));

  test('a retry after a wrong password confirms the same plan: the same opId, in place', async () => {
    // As a screen wires it: confirming executes the plan the dialog shows. Rust keeps the plan
    // after a wrong password, so the owner types again here and the same opId runs.
    const sent: { opId: unknown; password: unknown }[] = [];
    const answers = [
      () =>
        Promise.reject(
          refusal(DESKTOP_ERROR_CODES.AUTH_FAILED, {
            exhausted: false,
            messages: ['Authentication failure'],
          }),
        ),
      () => 1,
    ];
    mockIPC((cmd, args) => {
      if (cmd !== 'op_execute') return null;
      const { opId, password } = args as { opId: unknown; password?: unknown };
      sent.push({ opId, password });
      return answers.shift()?.();
    });
    try {
      const { user, onClose } = open({
        auth: pam,
        onConfirm: async (typed) => {
          await execute(7, typed);
        },
      });
      await user.type(password(), 'guess{Enter}');
      expect((await screen.findByRole('alert')).textContent).toBe('Authentication failure');
      expect(onClose).not.toHaveBeenCalled();
      expect(password().disabled).toBe(false);
      await user.type(password(), 'hunter2{Enter}');
      await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
      expect(sent).toEqual([
        { opId: 7, password: 'guess' },
        { opId: 7, password: 'hunter2' },
      ]);
    } finally {
      resetOperations();
      clearMocks();
    }
  });

  // Linux where polkit found no agent, or refused outside an active session: Rust keeps the
  // plan, app_info is read again and now offers the field, and the owner confirms the same plan
  // with the password.
  test.each([
    ['no_agent', 'The system could not show its password prompt.'],
    ['use_password_field', 'Use your system password here instead.'],
  ] as const)(
    '%s: the field the re-read brings confirms the same plan, same opId',
    async (reason, line) => {
      const polkit: AuthInfo = { ...pam, method: 'polkit', passwordField: false };
      const sent: { opId: unknown; password: unknown }[] = [];
      const answers = [
        () => Promise.reject(refusal(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason })),
        () => 1,
      ];
      mockIPC((cmd, args) => {
        if (cmd !== 'op_execute') return null;
        const { opId, password } = args as { opId: unknown; password?: unknown };
        sent.push({ opId, password });
        return answers.shift()?.();
      });
      const onClose = mock();
      const dialog = (auth: AuthInfo) => (
        <ConfirmDialog
          title="Remove target prod-eu"
          body="The target store forgets it. The cluster keeps running."
          confirmLabel="Remove"
          planClass="destructive"
          auth={auth}
          onConfirm={async (typed) => {
            await execute(7, typed);
          }}
          onClose={onClose}
        />
      );
      try {
        const { rerender } = render(dialog(polkit));
        const user = userEvent.setup();
        await user.click(confirm());
        expect((await screen.findByRole('alert')).textContent).toBe(line);
        expect(onClose).not.toHaveBeenCalled();
        // The PlatformGate read app_info again: the PAM route now.
        rerender(dialog(pam));
        await user.type(password(), 'hunter2{Enter}');
        await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
        expect(sent).toEqual([
          { opId: 7, password: undefined },
          { opId: 7, password: 'hunter2' },
        ]);
      } finally {
        resetOperations();
        clearMocks();
      }
    },
  );

  test('an expired password: changed meanwhile, a retry confirms the same plan, same opId', async () => {
    // Windows' credential dialog took the right password, expired: Rust keeps the plan, the
    // owner changes the password in the system with this dialog still open, and confirms again.
    const credential: AuthInfo = {
      ...hello,
      method: 'windows_credential',
      biometricsChoice: false,
    };
    const sent: unknown[] = [];
    const answers = [
      () =>
        Promise.reject(
          refusal(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'password_expired' }),
        ),
      () => 1,
    ];
    mockIPC((cmd, args) => {
      if (cmd !== 'op_execute') return null;
      sent.push((args as { opId: unknown }).opId);
      return answers.shift()?.();
    });
    try {
      const { user, onClose } = open({
        auth: credential,
        onConfirm: async (typed) => {
          await execute(7, typed);
        },
      });
      expect(screen.getByText('Confirm with your Windows password next.')).toBeDefined();
      await user.click(confirm());
      expect((await screen.findByRole('alert')).textContent).toBe(
        'Your system password has expired. Change it, then try again.',
      );
      expect(onClose).not.toHaveBeenCalled();
      expect(confirm().disabled).toBe(false);
      await user.click(confirm());
      await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
      expect(sent).toEqual([7, 7]);
    } finally {
      resetOperations();
      clearMocks();
    }
  });

  test('busy: a check is already open', async () => {
    const { user } = open({
      auth: pam,
      onConfirm: () => Promise.reject(refusal(DESKTOP_ERROR_CODES.AUTH_BUSY)),
    });
    await user.type(password(), 'hunter2{Enter}');
    expect((await screen.findByRole('alert')).textContent).toBe(
      'A check is already open. Finish it, then try again.',
    );
  });

  test('a refusal that is not about the password is shown as the error it is', async () => {
    const { user } = open({
      auth: pam,
      onConfirm: () => Promise.reject(refusal(DESKTOP_ERROR_CODES.PLAN_EXPIRED)),
    });
    await user.type(password(), 'hunter2{Enter}');
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain(`Rust says ${DESKTOP_ERROR_CODES.PLAN_EXPIRED}`);
    expect(alert.textContent).toContain(DESKTOP_ERROR_CODES.PLAN_EXPIRED);
  });
});
