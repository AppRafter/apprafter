// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { type KeyboardEvent, useRef } from 'react';
import type { Icon } from './icons';

export interface SegmentOption<V extends string> {
  readonly value: V;
  readonly label: string;
  readonly icon?: Icon;
  /** Shown, but not offered (O7: Auto-lock's Never while secrets are protected). */
  readonly disabled?: boolean;
  /** Why, as the option's tooltip. */
  readonly title?: string;
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

  /** An option the person may choose: there, and not one the caller withholds. */
  const offered = (index: number) => {
    const option = options[index];
    return option !== undefined && option.disabled !== true;
  };
  /** The next offered option from `from`, `by` one way, wrapping; `from` when none is. */
  const next = (from: number, by: 1 | -1) => {
    let at = from;
    for (let k = 0; k < options.length; k += 1) {
      at = (at + by + options.length) % options.length;
      if (offered(at)) return at;
    }
    return from;
  };
  /** The first and last offered options (`-1` when none is: Home and End then do nothing). */
  const firstOffered = options.findIndex((_, index) => offered(index));
  let lastOffered = -1;
  options.forEach((_, index) => {
    if (offered(index)) lastOffered = index;
  });

  const select = (index: number) => {
    const option = options[index];
    if (option === undefined || !offered(index)) return;
    if (option.value !== value) onChange(option.value);
    buttons.current[index]?.focus();
  };

  const onKeyDown = (event: KeyboardEvent, index: number) => {
    const target = {
      ArrowRight: next(index, 1),
      ArrowDown: next(index, 1),
      ArrowLeft: next(index, -1),
      ArrowUp: next(index, -1),
      Home: firstOffered,
      End: lastOffered,
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
            tabIndex={on || (checked === -1 && index === firstOffered) ? 0 : -1}
            disabled={disabled}
            // Not `disabled`: a disabled button shows no tooltip, and the reason must show (O7).
            aria-disabled={option.disabled === true || undefined}
            title={option.title}
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
