// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import { PlannedSection } from './PlannedSection';

test('a planned section has its page title and copy, and names its slice', () => {
  render(<PlannedSection section="backups" target="prod-eu" />);
  expect(screen.getByRole('heading', { level: 1, name: 'Backups' })).toBeDefined();
  expect(
    screen.getByText(
      'Encrypted off-site snapshots of claims data, platform objects and secrets. A restore replays one onto this or a fresh cluster.',
    ),
  ).toBeDefined();
  expect(screen.getByRole('heading', { name: 'Backups arrive in D.11.' })).toBeDefined();
  expect(screen.getByText('apprafter backup status')).toBeDefined();
});

test('a command that runs on the CLI’s active target says to switch to the tab’s first', () => {
  const { container } = render(<PlannedSection section="apps" target="prod-eu" />);
  expect(container.querySelector('.page-sub')).toBeNull();
  expect(screen.getByRole('heading', { name: 'Applications arrive in D.6.' })).toBeDefined();
  expect(container.querySelector('.state-text')?.textContent).toBe(
    'Until then the CLI does it, on its active target: run apprafter target use prod-eu first.',
  );
  expect(screen.getByText('apprafter target use prod-eu').tagName).toBe('CODE');
  expect(container.querySelector('.state-meta')?.textContent).toBe('apprafter app list');
});
