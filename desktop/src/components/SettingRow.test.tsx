// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import { CardRow, SettingRow } from './SettingRow';
import { Switch } from './Switch';

test('a row shows its label, sub, value and control; disabled, it dims', () => {
  render(
    <>
      <SettingRow
        label="Lock on start"
        sub="Ask for the owner when the app starts."
        control={<Switch label="Lock on start" checked onChange={() => {}} disabled />}
        disabled
      />
      <CardRow label="Server type" value="cx32" />
    </>,
  );
  const row = screen.getByText('Lock on start').closest('.row') as HTMLElement;
  expect(row.dataset.layout).toBe('setting');
  expect(row.dataset.disabled).toBe('true');
  expect(screen.getByText('Ask for the owner when the app starts.')).toBeDefined();
  expect(screen.getByRole('switch', { name: 'Lock on start' })).toBeDefined();
  const card = screen.getByText('Server type').closest('.row') as HTMLElement;
  expect(card.dataset.layout).toBe('card');
  expect(card.dataset.disabled).toBeUndefined();
  expect(screen.getByText('cx32').className).toBe('row-value');
});
