// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { act, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { onRadioHomeEnd } from './radioKeys';

function Group({ onChange }: { onChange: (value: string) => void }) {
  const radio = (name: string, value: string, disabled = false) => (
    <input
      type="radio"
      name={name}
      value={value}
      aria-label={value}
      disabled={disabled}
      onChange={() => onChange(value)}
    />
  );
  return (
    <fieldset onKeyDown={onRadioHomeEnd}>
      <legend>Group</legend>
      {radio('one', 'first-off', true)}
      {radio('one', 'a')}
      {radio('one', 'b')}
      {radio('two', 'other-group')}
      {radio('one', 'c')}
      {radio('one', 'last-off', true)}
      {/* A field that shares the group's name: only a radio's keys are taken. */}
      <input aria-label="Path" name="one" />
    </fieldset>
  );
}

const radio = (name: string) => screen.getByRole('radio', { name }) as HTMLInputElement;

function setup() {
  const onChange = mock();
  render(<Group onChange={onChange} />);
  return { onChange, user: userEvent.setup() };
}

describe('onRadioHomeEnd', () => {
  test('End chooses and focuses the last radio of the group that can be chosen', async () => {
    const { onChange, user } = setup();
    act(() => radio('a').focus());
    await user.keyboard('{End}');
    expect(document.activeElement).toBe(radio('c'));
    expect(onChange.mock.calls).toEqual([['c']]);
  });

  test('Home chooses and focuses the first one, past a disabled radio', async () => {
    const { onChange, user } = setup();
    act(() => radio('c').focus());
    await user.keyboard('{Home}');
    expect(document.activeElement).toBe(radio('a'));
    expect(onChange.mock.calls).toEqual([['a']]);
  });

  test('a radio of another group is never the target', async () => {
    const { onChange, user } = setup();
    act(() => radio('other-group').focus());
    await user.keyboard('{End}');
    expect(document.activeElement).toBe(radio('other-group'));
    expect(onChange.mock.calls).toEqual([['other-group']]);
  });

  test('Home and End in a text field stay the field’s own', async () => {
    const { onChange, user } = setup();
    const field = screen.getByLabelText('Path');
    act(() => field.focus());
    await user.keyboard('{Home}{End}');
    expect(document.activeElement).toBe(field);
    expect(onChange).not.toHaveBeenCalled();
  });

  // fireEvent, not user-event: user-event's own default for an End left to the engine throws on
  // a radio ("Not implemented"). The event not being cancelled is what is asserted.
  test('with a modifier held the keys are left alone', () => {
    const { onChange } = setup();
    act(() => radio('a').focus());
    for (const modifier of ['altKey', 'ctrlKey', 'metaKey', 'shiftKey']) {
      expect(fireEvent.keyDown(radio('a'), { key: 'End', [modifier]: true })).toBe(true);
    }
    expect(document.activeElement).toBe(radio('a'));
    expect(onChange).not.toHaveBeenCalled();
  });
});
