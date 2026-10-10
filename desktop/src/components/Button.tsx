// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ButtonHTMLAttributes, Ref } from 'react';
import type { Icon } from './icons';

export type ButtonVariant = 'primary' | 'secondary' | 'danger' | 'danger-solid' | 'ghost';
export type ButtonSize = 26 | 28 | 30 | 32 | 36;

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'type'> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: Icon;
  /** `button` unless it submits a form. */
  type?: 'button' | 'submit';
  /** Full width (the lock screen's Unlock). */
  full?: boolean;
  /** The button element, to move the focus to it. */
  ref?: Ref<HTMLButtonElement>;
  /**
   * Waiting on what it started (Check again, Run again, Verify): it looks disabled and a press
   * does nothing, but it stays focusable — a browser drops the focus of a control it disables
   * onto the page, out of reach of the dialog's Esc and Tab (review #1).
   */
  pending?: boolean;
}

/** The design's B(): primary, secondary, danger (outline), danger-solid, ghost (row action). */
export function Button({
  variant = 'secondary',
  size = 30,
  icon: IconComponent,
  type = 'button',
  full = false,
  pending = false,
  children,
  onClick,
  ...rest
}: ButtonProps) {
  return (
    <button
      {...rest}
      type={type}
      className="btn"
      data-variant={variant}
      data-size={size}
      data-full={full || undefined}
      aria-disabled={pending || undefined}
      onClick={(event) => {
        if (pending) {
          event.preventDefault();
          return;
        }
        onClick?.(event);
      }}
    >
      {IconComponent && <IconComponent aria-hidden="true" />}
      {children}
    </button>
  );
}
