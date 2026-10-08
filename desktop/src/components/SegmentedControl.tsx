// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { type KeyboardEvent, useRef } from 'react';
import type { Icon } from './icons';

export interface SegmentOption<V extends string> {
  readonly value: V;
  readonly label: string;
  readonly icon?: Icon;
}

export interface SegmentedControlProps<V extends string> {
  value: V;
  options: readonly SegmentOption<V>[];
  onChange: (value: V) => void;
  /** The group's name; or `labelledBy`, the id of a visible label. */
  ariaLabel?: string;
  labelledBy?: string;
  size?: 26 | 28;
  font?: 'mono' | 'sans';
  disabled?: boolean;
}

/**
 * One choice of a few, as a radio group: the checked option is the single tab stop, and the
 * arrow keys (Home, End) move the selection and the focus together, wrapping at the ends.
 */
export function SegmentedControl<V extends string>({
  value,
  options,
  onChange,
  ariaLabel,
  labelledBy,
  size = 28,
  font = 'sans',
  disabled = false,
}: SegmentedControlProps<V>) {
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);
  const checked = options.findIndex((o) => o.value === value);

  const select = (index: number) => {
    const option = options[index];
    if (option === undefined) return;
    if (option.value !== value) onChange(option.value);
    buttons.current[index]?.focus();
  };

  const onKeyDown = (event: KeyboardEvent, index: number) => {
    const last = options.length - 1;
    const target = {
      ArrowRight: index === last ? 0 : index + 1,
      ArrowDown: index === last ? 0 : index + 1,
      ArrowLeft: index === 0 ? last : index - 1,
      ArrowUp: index === 0 ? last : index - 1,
      Home: 0,
      End: last,
    }[event.key];
    if (target === undefined) return;
    event.preventDefault();
    select(target);
  };

  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      aria-labelledby={labelledBy}
      className="seg"
      data-size={size}
      data-font={font}
    >
      {options.map((option, index) => {
        const on = index === checked;
        const OptionIcon = option.icon;
        return (
          // biome-ignore lint/a11y/useSemanticElements: the ARIA radio-group pattern on a row of buttons (roving tabindex); native radios would need hidden inputs under styled labels
          <button
            key={option.value}
            ref={(element) => {
              buttons.current[index] = element;
            }}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={on || (checked === -1 && index === 0) ? 0 : -1}
            disabled={disabled}
            onClick={() => select(index)}
            onKeyDown={(event) => onKeyDown(event, index)}
          >
            {OptionIcon && <OptionIcon aria-hidden="true" />}
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
