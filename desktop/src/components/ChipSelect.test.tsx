// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, mock, test } from 'bun:test';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { ChipSelect } from './ChipSelect';

const REGIONS = [
  { value: 'hel1', label: 'hel1', secondary: 'Helsinki', meta: '12 ms' },
  { value: 'nbg1', label: 'nbg1', secondary: 'Nuremberg', meta: '38 ms' },
  { value: 'sin', label: 'sin', secondary: 'Singapore', meta: '–', disabled: true },
];

test('one radio per chip, named by its label, its secondary text and meta shown', () => {
  render(<ChipSelect legend="Region" value="nbg1" options={REGIONS} onChange={() => {}} />);
  expect(screen.getByRole('group', { name: 'Region' })).toBeDefined();
  expect((screen.getByRole('radio', { name: /^nbg1/ }) as HTMLInputElement).checked).toBe(true);
  expect(screen.getByText('Helsinki')).toBeDefined();
  expect(screen.getByText('12 ms')).toBeDefined();
});

test('the order given is the order shown', () => {
  render(<ChipSelect legend="Region" value={null} options={REGIONS} onChange={() => {}} />);
  const names = screen.getAllByRole('radio').map((r) => (r as HTMLInputElement).value);
  expect(names).toEqual(['hel1', 'nbg1', 'sin']);
});

test('a click chooses; a disabled chip does not', async () => {
  const onChange = mock();
  render(<ChipSelect legend="Region" value={null} options={REGIONS} onChange={onChange} />);
  const user = userEvent.setup();
  await user.click(screen.getByText('Helsinki'));
  await user.click(screen.getByText('Singapore'));
  expect(onChange.mock.calls).toEqual([['hel1']]);
  expect((screen.getByRole('radio', { name: /^sin/ }) as HTMLInputElement).disabled).toBe(true);
});

test('the chips are one group: one name, so the arrow keys move within them', () => {
  render(<ChipSelect legend="Region" value={null} options={REGIONS} onChange={() => {}} />);
  const names = new Set(screen.getAllByRole('radio').map((r) => (r as HTMLInputElement).name));
  expect(names.size).toBe(1);
  expect([...names][0]).not.toBe('');
});

test('a chip without secondary text or meta shows just its label', () => {
  render(
    <ChipSelect
      legend="Region"
      value={null}
      options={[{ value: 'fsn1', label: 'fsn1' }]}
      onChange={() => {}}
    />,
  );
  const chip = screen.getByRole('radio', { name: 'fsn1' }).closest('label');
  expect(chip?.querySelector('.chip-option-secondary')).toBeNull();
  expect(chip?.querySelector('.chip-option-meta')).toBeNull();
});

test('End skips the disabled chip to the last one that can be chosen; Home goes back', async () => {
  const onChange = mock();
  function Harness() {
    const [value, setValue] = useState<string>('nbg1');
    return (
      <ChipSelect
        legend="Region"
        value={value}
        options={REGIONS}
        onChange={(next) => {
          onChange(next);
          setValue(next);
        }}
      />
    );
  }
  render(<Harness />);
  const user = userEvent.setup();
  act(() => screen.getByRole('radio', { name: /^nbg1/ }).focus());
  await user.keyboard('{Home}');
  expect(document.activeElement).toBe(screen.getByRole('radio', { name: /^hel1/ }));
  await user.keyboard('{End}');
  expect(document.activeElement).toBe(screen.getByRole('radio', { name: /^nbg1/ }));
  expect(onChange.mock.calls).toEqual([['hel1'], ['nbg1']]);
});
