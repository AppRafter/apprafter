// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ButtonHTMLAttributes } from 'react';
import type { Icon } from './icons';

export interface IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'type' | 'aria-label' | 'children'> {
  /** The accessible name, also shown as the tooltip: an icon alone names nothing. */
  label: string;
  icon: Icon;
  size?: 22 | 26 | 28 | 30 | 34 | 36;
  tone?: 'default' | 'danger';
  /** A toggle's state (aria-pressed); leave out for a plain button. */
  pressed?: boolean;
}

export function IconButton({
  label,
  icon: IconComponent,
  size = 28,
  tone = 'default',
  pressed,
  title,
  ...rest
}: IconButtonProps) {
  return (
    <button
      {...rest}
      type="button"
      className="icon-btn"
      aria-label={label}
      title={title ?? label}
      aria-pressed={pressed}
      data-size={size}
      data-tone={tone}
    >
      <IconComponent aria-hidden="true" />
    </button>
  );
}
