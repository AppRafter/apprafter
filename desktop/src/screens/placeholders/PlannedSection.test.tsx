// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import { PlannedSection } from './PlannedSection';

test('a planned section has its page title and copy, names its slice and the CLI meanwhile', () => {
  render(<PlannedSection section="target" target="prod-eu" />);
  expect(screen.getByRole('heading', { level: 1, name: 'Target' })).toBeDefined();
  expect(
    screen.getByText(
      'How this computer reaches prod-eu — provider credentials, the machine, and cached access to the cluster.',
    ),
  ).toBeDefined();
  expect(screen.getByRole('heading', { name: 'The target screen arrives in D.3.' })).toBeDefined();
  expect(screen.getByText('apprafter target show')).toBeDefined();
});

test('a section the design gives no subtitle has none', () => {
  const { container } = render(<PlannedSection section="apps" target="prod-eu" />);
  expect(container.querySelector('.page-sub')).toBeNull();
  expect(screen.getByRole('heading', { name: 'Applications arrive in D.6.' })).toBeDefined();
});
