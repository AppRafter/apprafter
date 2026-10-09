// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { Modal } from './Modal';

function Harness({ dismissOnBackdrop = false }: { dismissOnBackdrop?: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <main>
        <button type="button" onClick={() => setOpen(true)}>
          Open settings
        </button>
      </main>
      {open && (
        <Modal
          title="Settings"
          width={620}
          dismissOnBackdrop={dismissOnBackdrop}
          onClose={() => setOpen(false)}
        >
          <button type="button">First</button>
          <button type="button">Second</button>
        </Modal>
      )}
    </>
  );
}

async function openIt(dismissOnBackdrop?: boolean) {
  const user = userEvent.setup();
  render(<Harness dismissOnBackdrop={dismissOnBackdrop ?? false} />);
  await user.click(screen.getByRole('button', { name: 'Open settings' }));
  return user;
}

const button = (name: string) => screen.getByRole('button', { name });

describe('Modal', () => {
  test('is a modal dialog named by its title, with the focus inside', async () => {
    await openIt();
    const dialog = screen.getByRole('dialog', { name: 'Settings' });
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    expect(document.activeElement).toBe(button('First'));
  });

  test('Tab and Shift+Tab stay inside, wrapping at both ends', async () => {
    const user = await openIt();
    await user.tab();
    expect(document.activeElement).toBe(button('Second'));
    await user.tab();
    expect(document.activeElement).toBe(button('Close'));
    await user.tab({ shift: true });
    expect(document.activeElement).toBe(button('Second'));
  });

  test('the background is inert while it is open, and only then', async () => {
    const user = await openIt();
    const main = document.querySelector('main') as HTMLElement;
    expect(main.hasAttribute('inert')).toBe(true);
    await user.click(button('Close'));
    expect(main.hasAttribute('inert')).toBe(false);
  });

  test('Esc closes it and the focus goes back where it was', async () => {
    const user = await openIt();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(button('Open settings'));
  });

  test('a backdrop click does nothing unless asked to, and leaves the focus inside', async () => {
    const user = await openIt();
    await user.click(document.querySelector('.modal-layer') as HTMLElement);
    expect(screen.queryByRole('dialog')).not.toBeNull();
    expect(document.activeElement).toBe(button('First'));
  });

  test('asked to, a backdrop click closes it; a click inside never does', async () => {
    const user = await openIt(true);
    await user.click(screen.getByRole('dialog'));
    expect(screen.queryByRole('dialog')).not.toBeNull();
    await user.click(document.querySelector('.modal-layer') as HTMLElement);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
