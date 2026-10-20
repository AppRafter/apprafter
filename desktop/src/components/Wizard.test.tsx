// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { Wizard, type WizardProps } from './Wizard';

const base = (): WizardProps => ({
  title: 'Add target',
  steps: ['Provider', 'Machine', 'Details'],
  step: 1,
  hint: 'Prices excl. VAT',
  onBack: mock(),
  nextLabel: 'Continue',
  onNext: mock(),
  onClose: mock(),
  children: <input aria-label="Field" />,
});

const host = (wizard: ReactNode) => <div style={{ position: 'relative' }}>{wizard}</div>;

/** The plan's helper; `withoutSteps` leaves the key out (exactOptionalPropertyTypes). */
function wizard(more: Partial<WizardProps> = {}, withoutSteps = false) {
  const props: WizardProps = { ...base(), ...more };
  const { steps: _steps, ...rest } = props;
  render(host(<Wizard {...(withoutSteps ? rest : props)} />));
  return { user: userEvent.setup(), ...props };
}

/** A wizard whose props a test changes between renders. */
function stepping(first: Partial<WizardProps> = {}) {
  let props: WizardProps = { ...base(), ...first };
  const view = render(host(<Wizard {...props} />));
  return {
    user: userEvent.setup(),
    update(more: Partial<WizardProps>) {
      props = { ...props, ...more };
      view.rerender(host(<Wizard {...props} />));
    },
  };
}

const button = (name: string) => screen.getByRole('button', { name }) as HTMLButtonElement;

describe('Wizard', () => {
  test('a dialog named by its title; the stepper marks the step done, current and to come', () => {
    wizard();
    const dialog = screen.getByRole('dialog', { name: 'Add target' });
    const steps = within(within(dialog).getByRole('list', { name: 'Steps' })).getAllByRole(
      'listitem',
    );
    expect(steps.map((s) => s.getAttribute('data-state'))).toEqual(['done', 'current', 'todo']);
    expect(steps[1]?.getAttribute('aria-current')).toBe('step');
    expect(steps[0]?.hasAttribute('aria-current')).toBe(false);
    expect(steps[0]?.textContent).toContain('(done)');
    expect(steps[2]?.textContent).not.toContain('(done)');
    expect(screen.getByText('Prices excl. VAT')).toBeDefined();
  });

  test('Back on a later step; none on the first or without onBack', async () => {
    const { user, onBack } = wizard();
    await user.click(button('Back'));
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  test('no Back on the first step', () => {
    wizard({ step: 0 });
    expect(screen.queryByRole('button', { name: 'Back' })).toBeNull();
  });

  test('Enter in a field goes next', async () => {
    const first = wizard();
    await first.user.type(screen.getByLabelText('Field'), 'x{Enter}');
    expect(first.onNext).toHaveBeenCalledTimes(1);
  });

  test('the Next button goes next', async () => {
    const { user, onNext } = wizard();
    await user.click(button('Continue'));
    expect(onNext).toHaveBeenCalledTimes(1);
  });

  test('a double click on Next goes next once: its second click is not a second Next', async () => {
    const { user, onNext } = wizard();
    await user.dblClick(button('Continue'));
    expect(onNext).toHaveBeenCalledTimes(1);
  });

  test('a held Enter goes next once: its repeats are not more Nexts', async () => {
    const { user, onNext } = wizard();
    await user.click(screen.getByLabelText('Field'));
    await user.keyboard('{Enter>3/}');
    expect(onNext).toHaveBeenCalledTimes(1);
  });

  test('disabled Next: neither the button nor Enter goes next', async () => {
    const { user, onNext } = wizard({ nextDisabled: true });
    await user.type(screen.getByLabelText('Field'), '{Enter}');
    expect(onNext).not.toHaveBeenCalled();
    expect(button('Continue').disabled).toBe(true);
  });

  // Enter alone cannot prove the guard: an engine does not submit on Enter while the form's
  // default button is disabled, so the submit handler never runs. A submit that does not come
  // from that button (a script's requestSubmit, an engine that differs) must not go next either.
  test('a submit that does not come from Next goes next only while Next can', () => {
    const cases: [Partial<WizardProps>, number][] = [
      [{ nextDisabled: true }, 0],
      [{ busy: true }, 0],
      [{}, 1],
    ];
    for (const [more, calls] of cases) {
      const { onNext } = wizard(more);
      const form = screen.getByRole('dialog').querySelector('form') as HTMLFormElement;
      // Cancelled every time: a submit left to the webview would load the page anew.
      expect(fireEvent.submit(form)).toBe(false);
      expect(onNext, JSON.stringify(more)).toHaveBeenCalledTimes(calls);
      cleanup();
    }
  });

  test('busy: Back and Close are disabled, Next waits, and Esc does not close', async () => {
    const { user, onClose, onNext } = wizard({ busy: true });
    for (const name of ['Back', 'Close']) {
      expect(button(name).disabled).toBe(true);
    }
    // Next keeps a focus it was pressed with: it waits (aria-disabled), a press does nothing.
    expect(button('Continue').disabled).toBe(false);
    expect(button('Continue').getAttribute('aria-disabled')).toBe('true');
    await user.click(button('Continue'));
    expect(onNext).not.toHaveBeenCalled();
    await user.keyboard('{Escape}');
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog').querySelector('form')?.getAttribute('aria-busy')).toBe(
      'true',
    );
  });

  test('busy: Enter in a field does not go next', async () => {
    const { user, onNext } = wizard({ busy: true });
    await user.type(screen.getByLabelText('Field'), '{Enter}');
    expect(onNext).not.toHaveBeenCalled();
  });

  test('not busy: Esc and Close close it', async () => {
    const { user, onClose } = wizard();
    await user.keyboard('{Escape}');
    await user.click(button('Close'));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  test('without steps there is no stepper (Change machine)', () => {
    wizard({ title: 'Change machine · staging' }, true);
    expect(screen.getByRole('dialog', { name: 'Change machine · staging' })).toBeDefined();
    expect(screen.queryByRole('list', { name: 'Steps' })).toBeNull();
    expect(screen.queryByRole('status')).toBeNull();
  });

  test('the step is announced: a polite status names it, and its place', () => {
    const { update } = stepping({ step: 0 });
    const status = screen.getByRole('status');
    expect(status.getAttribute('aria-live')).toBe('polite');
    expect(status.textContent).toBe('Step 1 of 3: Provider');
    update({ step: 1 });
    expect(screen.getByRole('status').textContent).toBe('Step 2 of 3: Machine');
  });
});

describe('Wizard focus', () => {
  test('a new step takes the focus to its first control', async () => {
    const { user, update } = stepping({ step: 0 });
    await user.click(button('Continue'));
    update({ step: 1, children: <input aria-label="Second" /> });
    expect(document.activeElement).toBe(screen.getByLabelText('Second'));
  });

  // Step 0's field and step 1's first radio are both an <input> in the same place: without a
  // new body per step React would turn the focused field into that radio, and the focus with it.
  test('a new step that is a radio group takes the focus to its chosen radio', () => {
    const { update } = stepping({ step: 0 });
    update({
      step: 1,
      children: (
        <>
          <input type="radio" name="tier" aria-label="Solo" readOnly />
          <input type="radio" name="tier" aria-label="Team" checked readOnly />
        </>
      ),
    });
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'Team' }));
  });

  test('a control of the new step that took the focus itself keeps it', () => {
    const { update } = stepping({ step: 0 });
    update({
      step: 1,
      children: (
        <>
          <input aria-label="First" />
          {/* biome-ignore lint/a11y/noAutofocus: the case under test: a step that focuses its own field */}
          <input aria-label="Name" autoFocus />
        </>
      ),
    });
    expect(document.activeElement).toBe(screen.getByLabelText('Name'));
  });

  test('a step with no control puts the focus on the dialog', () => {
    const { update } = stepping({ step: 0 });
    update({ step: 1, children: <p>Nothing to fill in.</p>, nextDisabled: true });
    const dialog = screen.getByRole('dialog');
    // The footer's Back is a control, but not the step's: the dialog takes it, not Back.
    expect(document.activeElement).toBe(dialog);
  });

  test('busy: Next keeps the focus it was pressed with, through busy and after', async () => {
    const { user, update } = stepping();
    await user.click(button('Continue'));
    expect(document.activeElement === button('Continue')).toBe(true);
    update({ busy: true });
    expect(button('Continue').disabled).toBe(false);
    expect(document.activeElement === button('Continue')).toBe(true);
    update({ busy: false });
    expect(document.activeElement === button('Continue')).toBe(true);
  });

  test('…and when busy ends on the next step, the focus goes to that step', async () => {
    const { user, update } = stepping({ step: 0 });
    await user.click(button('Continue'));
    update({ busy: true });
    update({ busy: false, step: 1, children: <input aria-label="Name" /> });
    expect(document.activeElement === screen.getByLabelText('Name')).toBe(true);
  });

  test('…and when busy ends with Next unable to go on, the focus goes to the step', async () => {
    const { user, update } = stepping();
    await user.click(button('Continue'));
    update({ busy: true });
    update({ busy: false, nextDisabled: true });
    expect(button('Continue').disabled).toBe(true);
    expect(document.activeElement === screen.getByLabelText('Field')).toBe(true);
  });

  test('busy: a focus lost to the page comes back to the step when it ends', () => {
    const { update } = stepping();
    update({ busy: true });
    act(() => {
      const elsewhere = document.createElement('input');
      document.body.append(elsewhere);
      elsewhere.focus();
      elsewhere.remove();
    });
    expect(document.activeElement).toBe(document.body);
    update({ busy: false });
    expect(document.activeElement).toBe(screen.getByLabelText('Field'));
  });

  test('a field that has the focus keeps it through busy', () => {
    const { update } = stepping();
    const field = screen.getByLabelText('Field');
    act(() => field.focus());
    update({ busy: true });
    expect(document.activeElement).toBe(field);
    update({ busy: false });
    expect(document.activeElement).toBe(field);
  });

  test("a control that goes while it has the focus hands it to the step's first control", async () => {
    const { user, update } = stepping({
      children: (
        <>
          <button type="button">Use another token</button>
          <input aria-label="Field" />
        </>
      ),
    });
    await user.click(button('Use another token'));
    update({ children: <input aria-label="Field" /> });
    expect(document.activeElement).toBe(screen.getByLabelText('Field'));
  });

  test('…and with no control left in the step, to the dialog, never the page', async () => {
    const { user, update } = stepping({
      children: <button type="button">Try again</button>,
    });
    await user.click(button('Try again'));
    update({ children: <p>Reading…</p> });
    expect(document.activeElement).toBe(screen.getByRole('dialog', { name: 'Add target' }));
  });

  test('a render that changes neither step nor busy leaves the focus alone', async () => {
    const { user, update } = stepping();
    await user.click(button('Continue'));
    update({ hint: 'Another hint' });
    expect(document.activeElement).toBe(button('Continue'));
  });
});
