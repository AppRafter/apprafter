// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { PasswordField } from './PasswordField';
import { TextField } from './TextField';

function Controlled({ secret }: { secret?: boolean }) {
  const [value, setValue] = useState('');
  return secret ? (
    <PasswordField label="API token" value={value} onChange={setValue} hint="Read and write." />
  ) : (
    <TextField label="Name" value={value} onChange={setValue} hint="Letters, digits, dashes." />
  );
}

describe('TextField', () => {
  test('its label names it and its hint describes it', () => {
    render(<Controlled />);
    const input = screen.getByLabelText('Name');
    const hint = screen.getByText('Letters, digits, dashes.');
    expect(input.getAttribute('aria-describedby')).toBe(hint.id);
  });

  test('typing reports the value', async () => {
    render(<Controlled />);
    await userEvent.setup().type(screen.getByLabelText('Name'), 'prod-eu');
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('prod-eu');
  });
});

describe('PasswordField', () => {
  test('hides the value until the reveal button is pressed, and hides it again', async () => {
    const user = userEvent.setup();
    render(<Controlled secret />);
    const input = screen.getByLabelText('API token') as HTMLInputElement;
    await user.type(input, 'hcloud-secret');
    expect(input.type).toBe('password');

    await user.click(screen.getByRole('button', { name: 'Show API token' }));
    expect(input.type).toBe('text');
    const hide = screen.getByRole('button', { name: 'Hide API token' });
    expect(hide.getAttribute('aria-pressed')).toBe('true');

    await user.click(hide);
    expect(input.type).toBe('password');
    expect(input.value).toBe('hcloud-secret');
  });
});
