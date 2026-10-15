// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StoreSecretForm, type StoreSecretFormProps } from './StoreSecretForm';

function show(more: Partial<StoreSecretFormProps> = {}) {
  const onSubmit = mock((_secret: string) => Promise.resolve());
  const onType = mock();
  const form = (props: Partial<StoreSecretFormProps>) => (
    <StoreSecretForm
      store="~/.config/apprafter"
      checking={false}
      backoff={false}
      refused={false}
      onType={onType}
      onSubmit={onSubmit}
      {...more}
      {...props}
    >
      <p>said</p>
    </StoreSecretForm>
  );
  const { rerender } = render(form({}));
  return {
    user: userEvent.setup(),
    onSubmit,
    onType,
    rerender: (props: Partial<StoreSecretFormProps>) => rerender(form(props)),
  };
}

/**
 * The form's own submit, past the disabled button: the guard in `submit` is what is tested
 * (critic A finding 8 — a click on a disabled button, or Enter, never reaches it).
 */
const submitForm = () => {
  const form = screen.getByLabelText('Lock password').closest('form');
  if (form === null) throw new Error('the field is in no form');
  fireEvent.submit(form);
};

describe('StoreSecretForm', () => {
  test('names the store, labels the field Lock password, takes either secret', async () => {
    const { user, onSubmit } = show();
    expect(screen.getByText('Target store ~/.config/apprafter')).toBeDefined();
    const field = screen.getByLabelText('Lock password') as HTMLInputElement;
    expect(field.placeholder).toBe('Lock password or recovery key');
    expect(field.type).toBe('password');
    await user.type(field, 'the lock password{Enter}');
    expect(onSubmit).toHaveBeenCalledWith('the lock password');
    await waitFor(() => expect(field.value).toBe(''));
  });

  test('nothing is sent empty', () => {
    const { onSubmit } = show();
    submitForm();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  test('nothing is sent while a check runs or the back-off holds', async () => {
    const { user, onSubmit, rerender } = show();
    await user.type(screen.getByLabelText('Lock password'), 'the lock password');
    rerender({ checking: true });
    expect((screen.getByLabelText('Lock password') as HTMLInputElement).readOnly).toBe(true);
    submitForm();
    expect(onSubmit).not.toHaveBeenCalled();
    rerender({ backoff: true });
    expect((screen.getByLabelText('Lock password') as HTMLInputElement).disabled).toBe(true);
    submitForm();
    expect(onSubmit).not.toHaveBeenCalled();
    rerender({});
    submitForm();
    expect(onSubmit).toHaveBeenCalledWith('the lock password');
  });

  test('D.4b\'s gesture sits under an "or", and D.4a shows none', () => {
    show();
    expect(screen.queryByText('or')).toBeNull();
    show({ gesture: <button type="button">Unlock with Windows Hello</button> });
    expect(screen.getByText('or')).toBeDefined();
    expect(screen.getByRole('button', { name: 'Unlock with Windows Hello' })).toBeDefined();
  });
});
