// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ButtonHTMLAttributes } from 'react';
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
}

/** The design's B(): primary, secondary, danger (outline), danger-solid, ghost (row action). */
export function Button({
  variant = 'secondary',
  size = 30,
  icon: IconComponent,
  type = 'button',
  full = false,
  children,
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
    >
      {IconComponent && <IconComponent aria-hidden="true" />}
      {children}
    </button>
  );
}
