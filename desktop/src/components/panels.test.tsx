// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// ErrorPanel, StatePanel, InfoSheet.
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import UI_ERRORS from '../ipc/generated/fixtures/ui-errors.json';
import type { UiError } from '../ipc/generated/UiError';
import { Button } from './Button';
import { ErrorPanel } from './ErrorPanel';
import { InfoSheet } from './InfoSheet';
import { SpinnerGapIcon } from './icons';
import { StatePanel } from './StatePanel';

const notFound: UiError = {
  code: 'apprafter::target::not_found',
  message: 'No target named prod-eu.',
  help: 'Add it with the target wizard.',
  causes: ['config.yaml has no entry prod-eu', 'the CLI default is staging'],
  fields: {},
};

describe('ErrorPanel', () => {
  test('shows the message, the code and the help; the causes open on request', async () => {
    const user = userEvent.setup();
    render(<ErrorPanel error={notFound} />);
    const panel = screen.getByRole('alert');
    expect(panel.textContent).toContain('No target named prod-eu.');
    expect(screen.getByText('apprafter::target::not_found')).toBeDefined();
    expect(screen.getByText('Add it with the target wizard.')).toBeDefined();
    const toggle = screen.getByRole('button', { name: 'Show 2 causes' });
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByText('the CLI default is staging')).toBeNull();
    await user.click(toggle);
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    expect(screen.getByText('the CLI default is staging')).toBeDefined();
  });

  test('a known code offers its action', async () => {
    const onAction = mock();
    render(<ErrorPanel error={notFound} onAction={onAction} />);
    await userEvent.setup().click(screen.getByRole('button', { name: 'Add a target' }));
    expect(onAction).toHaveBeenCalledWith({ kind: 'add-target' });
  });

  // WI-452: the core's real projections carry a source-neutral help — the diagnosis, never a
  // CLI command or flag the GUI does not have — and the panel shows it beside the action.
  test('the real projections show a help with no CLI command or flag', () => {
    for (const [name, ui] of Object.entries(UI_ERRORS as Record<string, UiError>)) {
      const { unmount } = render(<ErrorPanel error={ui} onAction={() => {}} />);
      const shown = screen.getByRole('alert').textContent ?? '';
      expect(shown, name).not.toContain('apprafter ');
      expect(shown, name).not.toMatch(/`[^`]*--/);
      if (ui.help !== null) expect(shown, name).toContain(ui.help.split('\n')[0] ?? '');
      unmount();
    }
    render(<ErrorPanel error={UI_ERRORS.tokenRejected as UiError} onAction={() => {}} />);
    const panel = screen.getByRole('alert');
    expect(panel.textContent).toContain('it was mistyped, or it was revoked or rotated');
    expect(screen.getByRole('button', { name: 'Renew token' })).toBeDefined();
  });

  test('an unknown code, or no handler, offers nothing', () => {
    render(
      <ErrorPanel error={{ ...notFound, code: 'apprafter::op::cancelled' }} onAction={() => {}} />,
    );
    render(<ErrorPanel error={notFound} />);
    expect(screen.queryByRole('button', { name: /target/ })).toBeNull();
  });
});

test('StatePanel shows its title, text, meta and actions; a spinner spins', () => {
  render(
    <StatePanel
      icon={SpinnerGapIcon}
      spin
      tone="warn"
      title="Provisioning prod-eu"
      text="The machine is being created."
      meta="about 6 min"
      actions={<Button>Cancel</Button>}
    />,
  );
  expect(screen.getByRole('heading', { name: 'Provisioning prod-eu' })).toBeDefined();
  expect(screen.getByText('The machine is being created.')).toBeDefined();
  expect(screen.getByText('about 6 min')).toBeDefined();
  expect(screen.getByRole('button', { name: 'Cancel' })).toBeDefined();
  expect(document.querySelector('.state-panel svg')?.getAttribute('class')).toContain('spin');
});

test('InfoSheet lists its rows as terms and closes on the backdrop', async () => {
  const onClose = mock();
  render(
    <InfoSheet
      title="Snapshot 7f3a"
      sub="read from the snapshot itself"
      rows={[
        { k: 'Cluster', v: 'prod-eu' },
        { k: 'Size', v: '1.2 GiB' },
      ]}
      onClose={onClose}
    />,
  );
  expect(screen.getByRole('dialog', { name: 'Snapshot 7f3a' })).toBeDefined();
  const terms = screen.getAllByRole('term').map((t) => t.textContent);
  const values = screen.getAllByRole('definition').map((d) => d.textContent);
  expect(terms).toEqual(['Cluster', 'Size']);
  expect(values).toEqual(['prod-eu', '1.2 GiB']);
  await userEvent.setup().click(document.querySelector('.modal-layer') as HTMLElement);
  expect(onClose).toHaveBeenCalledTimes(1);
});
