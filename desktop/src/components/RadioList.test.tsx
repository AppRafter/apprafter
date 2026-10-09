// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, mock, test } from 'bun:test';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { RadioList, type RadioListItem } from './RadioList';

const items = (field = <input aria-label="Path" />): RadioListItem<string>[] => [
  {
    value: 'a',
    ariaLabel: '~/.ssh/id_ed25519.pub',
    label: '~/.ssh/id_ed25519.pub',
    detail: 'ssh-ed25519',
    meta: 'alex@host',
  },
  { value: 'other', ariaLabel: 'Other path…', label: 'Other path…', expanded: field },
  { value: 'skip', ariaLabel: 'Skip', label: 'Skip', detail: 'No key.' },
];

/** The text of the elements an aria-describedby names, in order. */
function description(element: HTMLElement): string {
  return (element.getAttribute('aria-describedby') ?? '')
    .split(' ')
    .filter((id) => id !== '')
    .map((id) => document.getElementById(id)?.textContent ?? `<missing ${id}>`)
    .join(' ');
}

test('rows are radios named by ariaLabel, with detail and meta shown', () => {
  render(<RadioList legend="SSH public key" value="a" items={items()} onChange={() => {}} />);
  expect(screen.getByRole('group', { name: 'SSH public key' })).toBeDefined();
  expect(
    (screen.getByRole('radio', { name: '~/.ssh/id_ed25519.pub' }) as HTMLInputElement).checked,
  ).toBe(true);
  expect(screen.getByText('ssh-ed25519')).toBeDefined();
  expect(screen.getByText('alex@host')).toBeDefined();
});

test("a row's detail and meta describe its radio; a row with neither has no description", () => {
  render(<RadioList legend="SSH public key" value="a" items={items()} onChange={() => {}} />);
  expect(description(screen.getByRole('radio', { name: '~/.ssh/id_ed25519.pub' }))).toBe(
    'ssh-ed25519 alex@host',
  );
  expect(description(screen.getByRole('radio', { name: 'Skip' }))).toBe('No key.');
  expect(screen.getByRole('radio', { name: 'Other path…' }).hasAttribute('aria-describedby')).toBe(
    false,
  );
});

test("a row's expanded content shows only while it is chosen, outside its label", () => {
  const { rerender } = render(
    <RadioList legend="SSH public key" value="a" items={items()} onChange={() => {}} />,
  );
  expect(screen.queryByLabelText('Path')).toBeNull();
  rerender(<RadioList legend="SSH public key" value="other" items={items()} onChange={() => {}} />);
  const field = screen.getByLabelText('Path');
  expect(field.closest('label')).toBeNull();
});

test('choosing reports the value', async () => {
  const onChange = mock();
  render(<RadioList legend="SSH public key" value="a" items={items()} onChange={onChange} />);
  await userEvent.setup().click(screen.getByRole('radio', { name: 'Skip' }));
  expect(onChange).toHaveBeenCalledWith('skip');
});

test('a click on the row’s text chooses it; a disabled row cannot be chosen', async () => {
  const onChange = mock();
  const list = items().map((item) => (item.value === 'skip' ? { ...item, disabled: true } : item));
  render(<RadioList legend="SSH public key" value="a" items={list} onChange={onChange} />);
  const user = userEvent.setup();
  await user.click(screen.getByText('Other path…'));
  await user.click(screen.getByText('No key.'));
  expect(onChange.mock.calls).toEqual([['other']]);
  expect((screen.getByRole('radio', { name: 'Skip' }) as HTMLInputElement).disabled).toBe(true);
});

test('Home and End choose the first and last row', async () => {
  const onChange = mock();
  function Harness() {
    const [value, setValue] = useState<string>('other');
    return (
      <RadioList
        legend="SSH public key"
        value={value}
        items={items()}
        onChange={(next) => {
          onChange(next);
          setValue(next);
        }}
      />
    );
  }
  render(<Harness />);
  const user = userEvent.setup();
  act(() => screen.getByRole('radio', { name: 'Other path…' }).focus());
  await user.keyboard('{End}');
  expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'Skip' }));
  await user.keyboard('{Home}');
  expect(document.activeElement).toBe(screen.getByRole('radio', { name: '~/.ssh/id_ed25519.pub' }));
  expect(onChange.mock.calls).toEqual([['skip'], ['a']]);
});
