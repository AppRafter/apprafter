// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Switch } from './Switch';

describe('Switch', () => {
  test('is a named switch that reports its state', () => {
    render(<Switch label="Lock on start" checked onChange={() => {}} />);
    const control = screen.getByRole('switch', { name: 'Lock on start' });
    expect(control.getAttribute('aria-checked')).toBe('true');
  });

  test('a click or Space asks for the other state', async () => {
    const user = userEvent.setup();
    const onChange = mock();
    render(<Switch label="Lock on start" checked={false} onChange={onChange} />);
    const control = screen.getByRole('switch', { name: 'Lock on start' });
    await user.click(control);
    control.focus();
    await user.keyboard(' ');
    expect(onChange.mock.calls).toEqual([[true], [true]]);
  });

  test('disabled, it does not move', async () => {
    const onChange = mock();
    render(<Switch label="Lock on start" checked onChange={onChange} disabled />);
    const control = screen.getByRole('switch', { name: 'Lock on start' }) as HTMLButtonElement;
    expect(control.disabled).toBe(true);
    await userEvent.setup().click(control);
    expect(onChange).not.toHaveBeenCalled();
  });
});
