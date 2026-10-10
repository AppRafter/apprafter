// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Button } from './Button';
import { IconButton } from './IconButton';
import { LockSimpleIcon, XIcon } from './icons';

describe('Button', () => {
  test('is a real button that does not submit a form unless asked', async () => {
    const onClick = mock();
    render(<Button onClick={onClick}>Save</Button>);
    const button = screen.getByRole('button', { name: 'Save' });
    expect(button.getAttribute('type')).toBe('button');
    await userEvent.setup().click(button);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  test('disabled is the real attribute: no click gets through', async () => {
    const onClick = mock();
    render(
      <Button disabled onClick={onClick}>
        Save
      </Button>,
    );
    const button = screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    await userEvent.setup().click(button);
    expect(onClick).not.toHaveBeenCalled();
  });

  test('variant and size reach the stylesheet; the icon is decoration', () => {
    render(
      <Button variant="danger-solid" size={32} icon={LockSimpleIcon}>
        Lock
      </Button>,
    );
    const button = screen.getByRole('button', { name: 'Lock' });
    expect(button.dataset.variant).toBe('danger-solid');
    expect(button.dataset.size).toBe('32');
    expect(button.querySelector('svg')?.getAttribute('aria-hidden')).toBe('true');
  });
});

describe('IconButton', () => {
  test('is named by its label, which is also its tooltip', async () => {
    const onClick = mock();
    render(<IconButton label="Close" icon={XIcon} onClick={onClick} />);
    const button = screen.getByRole('button', { name: 'Close' });
    expect(button.getAttribute('title')).toBe('Close');
    await userEvent.setup().click(button);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  test('a toggle says whether it is pressed', () => {
    const { rerender } = render(<IconButton label="Show" icon={XIcon} pressed={false} />);
    expect(screen.getByRole('button', { name: 'Show' }).getAttribute('aria-pressed')).toBe('false');
    rerender(<IconButton label="Show" icon={XIcon} pressed />);
    expect(screen.getByRole('button', { name: 'Show' }).getAttribute('aria-pressed')).toBe('true');
  });

  test('a plain one has no pressed state', () => {
    render(<IconButton label="Close" icon={XIcon} />);
    expect(screen.getByRole('button', { name: 'Close' }).hasAttribute('aria-pressed')).toBe(false);
  });
});
