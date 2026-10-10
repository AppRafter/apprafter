// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { ChoiceCardGroup } from './ChoiceCardGroup';

const TIERS = [
  { value: 'solo', label: 'Solo (T1)' },
  { value: 'team', label: 'Team (T2)', sub: 'three or more nodes' },
  { value: 'prod', label: 'Pro (T3)', disabled: true },
] as const;

describe('ChoiceCardGroup', () => {
  test('a named group of radios, the chosen one checked', () => {
    render(<ChoiceCardGroup legend="Tier" value="team" options={TIERS} onChange={() => {}} />);
    const group = screen.getByRole('group', { name: 'Tier' });
    expect(group).toBeDefined();
    const team = screen.getByRole('radio', { name: /Team \(T2\)/ }) as HTMLInputElement;
    expect(team.checked).toBe(true);
    expect((screen.getByRole('radio', { name: /Solo/ }) as HTMLInputElement).checked).toBe(false);
  });

  test('choosing a card reports its value; a disabled card cannot be chosen', async () => {
    const onChange = mock();
    render(<ChoiceCardGroup legend="Tier" value={null} options={TIERS} onChange={onChange} />);
    const user = userEvent.setup();
    await user.click(screen.getByText('Solo (T1)'));
    expect(onChange).toHaveBeenCalledWith('solo');
    const pro = screen.getByRole('radio', { name: /Pro/ }) as HTMLInputElement;
    expect(pro.disabled).toBe(true);
    await user.click(screen.getByText('Pro (T3)'));
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  test('a hint below the cards describes the group', () => {
    render(
      <ChoiceCardGroup
        legend="Tier"
        value={null}
        options={TIERS}
        onChange={() => {}}
        hint="A hint only."
      />,
    );
    const hint = screen.getByText('A hint only.');
    const described = screen.getByRole('group', { name: 'Tier' }).getAttribute('aria-describedby');
    expect(described).toBe(hint.id);
  });

  test('no hint, no description', () => {
    render(<ChoiceCardGroup legend="Tier" value={null} options={TIERS} onChange={() => {}} />);
    expect(screen.getByRole('group', { name: 'Tier' }).hasAttribute('aria-describedby')).toBe(
      false,
    );
  });

  test('End chooses the last card that can be chosen, Home the first', async () => {
    const onChange = mock();
    function Harness() {
      const [value, setValue] = useState<string>('solo');
      return (
        <ChoiceCardGroup
          legend="Tier"
          value={value}
          options={TIERS}
          onChange={(next) => {
            onChange(next);
            setValue(next);
          }}
        />
      );
    }
    render(<Harness />);
    const user = userEvent.setup();
    act(() => screen.getByRole('radio', { name: /Solo/ }).focus());
    await user.keyboard('{End}');
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: /Team/ }));
    await user.keyboard('{Home}');
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: /Solo/ }));
    expect(onChange.mock.calls).toEqual([['team'], ['solo']]);
  });
});
