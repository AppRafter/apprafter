// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import { ConfirmDialog, type ConfirmDialogProps, needsDialog } from './ConfirmDialog';

const hello: AuthInfo = {
  available: true,
  method: 'windows_hello',
  unavailable: null,
  biometricsChoice: true,
  passwordField: false,
};

function open(props: Partial<ConfirmDialogProps> = {}) {
  const onConfirm = mock(() => Promise.resolve());
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
  return { user: userEvent.setup(), onConfirm, onClose };
}

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
