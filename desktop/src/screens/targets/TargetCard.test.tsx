// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, mock, test } from 'bun:test';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { targetSummary } from '../../test/fixtures';
import { TargetCard } from './TargetCard';

test('a card is an article with two buttons: open (or switch) and make default', async () => {
  const onOpen = mock();
  const onMakeDefault = mock();
  render(
    <TargetCard
      target={targetSummary({ name: 'staging', isCliDefault: false })}
      open={false}
      onOpen={onOpen}
      onMakeDefault={onMakeDefault}
    />,
  );
  const card = screen.getByRole('article', { name: 'staging' });
  expect(card.textContent).toContain('hetzner-cloud · nbg1');
  expect(card.textContent).toContain('Team (T2)');
  expect(card.textContent).toContain('server type cx22');
  const open = within(card).getByRole('button', { name: 'Open staging' });
  expect(open.textContent).toBe('Open in new tab');
  const user = userEvent.setup();
  await user.click(open);
  await user.click(within(card).getByRole('button', { name: 'Make staging the CLI default' }));
  expect(onOpen).toHaveBeenCalledWith('staging');
  expect(onMakeDefault).toHaveBeenCalledWith('staging');
});

test('the CLI default has its tag and no make-default button; an open target switches', () => {
  render(<TargetCard target={targetSummary()} open onOpen={() => {}} onMakeDefault={() => {}} />);
  const card = screen.getByRole('article', { name: 'prod-eu' });
  expect(within(card).getByText('CLI default')).toBeDefined();
  expect(within(card).queryByRole('button', { name: /CLI default/ })).toBeNull();
  expect(within(card).getByRole('button', { name: 'Switch to prod-eu' }).textContent).toBe(
    'Switch to tab',
  );
});

test('what is not set says so', () => {
  render(
    <TargetCard
      target={targetSummary({ region: null, serverType: null, defaultTier: null, tierLevel: null })}
      open={false}
      onOpen={() => {}}
      onMakeDefault={() => {}}
    />,
  );
  const card = screen.getByRole('article', { name: 'prod-eu' });
  expect(card.textContent).toContain('hetzner-cloud · region not set');
  expect(card.textContent).toContain('server type not set');
  expect(card.textContent).toContain('Default tier not set');
});
