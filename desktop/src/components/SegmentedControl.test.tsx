// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { DesktopIcon, MoonIcon, SunIcon } from './icons';
import { SegmentedControl } from './SegmentedControl';

type Theme = 'system' | 'light' | 'dark';
const OPTIONS = [
  { value: 'system', label: 'System', icon: DesktopIcon },
  { value: 'light', label: 'Light', icon: SunIcon },
  { value: 'dark', label: 'Dark', icon: MoonIcon },
] as const;

function Theme({ onChange, disabled }: { onChange?: (v: Theme) => void; disabled?: boolean }) {
  const [value, setValue] = useState<Theme>('light');
  return (
    <SegmentedControl
      ariaLabel="Theme"
      value={value}
      options={OPTIONS}
      disabled={disabled ?? false}
      onChange={(v) => {
        setValue(v);
        onChange?.(v);
      }}
    />
  );
}

const radio = (name: string) => screen.getByRole('radio', { name });

describe('SegmentedControl', () => {
  test('is a radio group: the checked option is the one tab stop', () => {
    render(<Theme />);
    expect(screen.getByRole('radiogroup', { name: 'Theme' })).toBeDefined();
    expect(radio('Light').getAttribute('aria-checked')).toBe('true');
    expect(radio('Dark').getAttribute('aria-checked')).toBe('false');
    expect([radio('System'), radio('Light'), radio('Dark')].map((r) => r.tabIndex)).toEqual([
      -1, 0, -1,
    ]);
  });

  test('a click selects', async () => {
    const onChange = mock();
    render(<Theme onChange={onChange} />);
    await userEvent.setup().click(radio('Dark'));
    expect(onChange).toHaveBeenCalledWith('dark');
    expect(radio('Dark').getAttribute('aria-checked')).toBe('true');
  });

  test('clicking the selected option changes nothing', async () => {
    const onChange = mock();
    render(<Theme onChange={onChange} />);
    await userEvent.setup().click(radio('Light'));
    expect(onChange).not.toHaveBeenCalled();
  });

  test('arrow keys move the selection and the focus, wrapping at the ends', async () => {
    const user = userEvent.setup();
    const onChange = mock();
    render(<Theme onChange={onChange} />);
    radio('Light').focus();
    await user.keyboard('{ArrowRight}');
    expect(document.activeElement).toBe(radio('Dark'));
    expect(radio('Dark').getAttribute('aria-checked')).toBe('true');
    await user.keyboard('{ArrowRight}');
    expect(document.activeElement).toBe(radio('System'));
    await user.keyboard('{ArrowLeft}');
    expect(document.activeElement).toBe(radio('Dark'));
    await user.keyboard('{Home}');
    expect(radio('System').getAttribute('aria-checked')).toBe('true');
    await user.keyboard('{End}');
    expect(radio('Dark').getAttribute('aria-checked')).toBe('true');
    expect(onChange.mock.calls.map((c) => c[0])).toEqual([
      'dark',
      'system',
      'dark',
      'system',
      'dark',
    ]);
  });

  test('disabled, nothing selects', async () => {
    const onChange = mock();
    render(<Theme onChange={onChange} disabled />);
    await userEvent.setup().click(radio('Dark'));
    expect(onChange).not.toHaveBeenCalled();
    expect((radio('Dark') as HTMLButtonElement).disabled).toBe(true);
  });

  test('an option not offered is shown with its reason, and neither a click nor a key reaches it', async () => {
    const onChange = mock();
    function AutoLock() {
      const [value, setValue] = useState<'5' | '30' | 'never'>('30');
      return (
        <SegmentedControl
          ariaLabel="Auto-lock"
          value={value}
          options={[
            { value: '5', label: '5 min' },
            { value: '30', label: '30 min' },
            {
              value: 'never',
              label: 'Never',
              disabled: true,
              title: 'Not while secrets are protected.',
            },
          ]}
          onChange={(v) => {
            setValue(v);
            onChange(v);
          }}
        />
      );
    }
    render(<AutoLock />);
    const user = userEvent.setup();
    const never = radio('Never');
    expect(never.getAttribute('aria-disabled')).toBe('true');
    expect(never.getAttribute('title')).toBe('Not while secrets are protected.');
    await user.click(never);
    expect(onChange).not.toHaveBeenCalled();
    expect(radio('30 min').getAttribute('aria-checked')).toBe('true');
    // From 30 min, the next option is Never, not offered: the arrow wraps to 5 min.
    radio('30 min').focus();
    await user.keyboard('{ArrowRight}');
    expect(onChange).toHaveBeenLastCalledWith('5');
    await user.keyboard('{End}');
    expect(onChange).toHaveBeenLastCalledWith('30');
  });
});
