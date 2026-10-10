// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { type ReactNode, useState } from 'react';
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
      <div className="toast-viewport" role="status" aria-live="polite" />
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

  test('a live region beside it stays live: a message is still heard over the dialog', async () => {
    await openIt();
    expect(document.querySelector('main')?.hasAttribute('inert')).toBe(true);
    expect(screen.getByRole('status').hasAttribute('inert')).toBe(false);
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

describe('the focus never falls out of an open dialog (review #0, #7)', () => {
  test("a footer's <summary> is a stop of the trap: Tab from it goes on, never back to the start", async () => {
    const user = userEvent.setup();
    const onClose = mock();
    render(
      <Modal
        title="Toolchain"
        width={620}
        onClose={onClose}
        footer={
          <>
            <details>
              <summary>Searched 2 directories</summary>
              <ul>
                <li>/usr/bin</li>
              </ul>
            </details>
            <button type="button">Check again</button>
          </>
        }
      >
        <p>No controls in the body.</p>
      </Modal>,
    );
    const summary = screen.getByText('Searched 2 directories');
    summary.focus();
    await user.tab();
    expect(document.activeElement).toBe(button('Check again'));
    summary.focus();
    await user.tab({ shift: true });
    expect(document.activeElement).toBe(button('Close'));
  });

  // Chromium 130 and later (WebView2 too) make a scroll container with no Tab stop of its own a
  // Tab stop, so a keyboard can scroll it; WebKit does not. happy-dom lays nothing out, so the
  // body is made to scroll here by hand: taller than its box, and overflow-y auto.
  function scrollingToolchain(rows: ReactNode = <p>kubectl, helm and restic found.</p>) {
    render(
      <Modal
        title="Toolchain"
        width={620}
        onClose={mock()}
        footer={
          <>
            <details>
              <summary>Searched 2 directories</summary>
              <ul>
                <li>/usr/bin</li>
              </ul>
            </details>
            <button type="button">Check again</button>
          </>
        }
      >
        {rows}
      </Modal>,
    );
    const body = screen.getByRole('dialog').querySelector<HTMLElement>('[data-modal-body]');
    if (body === null) throw new Error('no body');
    body.style.overflowY = 'auto';
    Object.defineProperty(body, 'scrollHeight', { configurable: true, value: 600 });
    Object.defineProperty(body, 'clientHeight', { configurable: true, value: 200 });
    return body;
  }

  test('a body that scrolls and holds no control is a stop: Tab goes Close, body, footer', async () => {
    const user = userEvent.setup();
    const body = scrollingToolchain();
    button('Close').focus();
    await user.tab();
    expect(document.activeElement === body).toBe(true);
    await user.tab();
    expect(document.activeElement === screen.getByText('Searched 2 directories')).toBe(true);
    await user.tab();
    expect(document.activeElement === button('Check again')).toBe(true);
    await user.tab();
    expect(document.activeElement === button('Close')).toBe(true);
    await user.tab({ shift: true });
    expect(document.activeElement === button('Check again')).toBe(true);
    await user.tab({ shift: true });
    await user.tab({ shift: true });
    expect(document.activeElement === body).toBe(true);
    await user.tab({ shift: true });
    expect(document.activeElement === button('Close')).toBe(true);
  });

  test('a body with a control of its own, or that no longer scrolls, is no stop', async () => {
    const user = userEvent.setup();
    const body = scrollingToolchain(<button type="button">Copy brew install restic</button>);
    button('Close').focus();
    await user.tab();
    expect(document.activeElement === button('Copy brew install restic')).toBe(true);
    expect(body.hasAttribute('tabindex')).toBe(false);
    cleanup();
    const again = scrollingToolchain();
    button('Close').focus();
    await user.tab();
    expect(document.activeElement === again).toBe(true);
    Object.defineProperty(again, 'scrollHeight', { configurable: true, value: 200 });
    button('Close').focus();
    await user.tab();
    expect(document.activeElement === screen.getByText('Searched 2 directories')).toBe(true);
    expect(again.hasAttribute('tabindex')).toBe(false);
  });

  test('content that overflows a box that clips it, not scrolls, is no stop', async () => {
    const user = userEvent.setup();
    const body = scrollingToolchain();
    body.style.overflowY = 'hidden';
    button('Close').focus();
    await user.tab();
    expect(document.activeElement === screen.getByText('Searched 2 directories')).toBe(true);
    expect(body.hasAttribute('tabindex')).toBe(false);
  });

  test('an element of its own tabindex keeps it: a scroller the page made unreachable stays so', async () => {
    const user = userEvent.setup();
    const body = scrollingToolchain();
    body.setAttribute('tabindex', '-1');
    button('Close').focus();
    await user.tab();
    expect(document.activeElement === screen.getByText('Searched 2 directories')).toBe(true);
    expect(body.getAttribute('tabindex')).toBe('-1');
  });

  test('Tab from what the trap does not list goes on from where it is, not back to the start', async () => {
    const user = userEvent.setup();
    render(
      <Modal title="Doctor" width={660} onClose={mock()}>
        <button type="button">Show the toolchain</button>
        <h3 tabIndex={-1}>This computer</h3>
        <button type="button">Copy report</button>
      </Modal>,
    );
    const heading = screen.getByRole('heading', { name: 'This computer' });
    heading.focus();
    await user.tab();
    expect(document.activeElement === button('Copy report')).toBe(true);
    heading.focus();
    await user.tab({ shift: true });
    expect(document.activeElement === button('Show the toolchain')).toBe(true);
  });

  test('a control that goes while it has the focus leaves it in the dialog, so Esc still closes it', async () => {
    const onClose = mock();
    function Gone() {
      const [shown, setShown] = useState(true);
      return (
        <Modal title="Doctor" width={660} onClose={onClose}>
          <p>Checking…</p>
          {shown && (
            <button type="button" onClick={() => setShown(false)}>
              Cancel
            </button>
          )}
        </Modal>
      );
    }
    const user = userEvent.setup();
    render(<Gone />);
    await user.click(button('Cancel'));
    const dialog = screen.getByRole('dialog', { name: 'Doctor' });
    expect(dialog.contains(document.activeElement)).toBe(true);
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
