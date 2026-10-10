// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';
import type { Tone } from './tone';

export interface StatusPillProps {
  tone: Tone;
  size?: 20 | 22;
  /** A 6px dot in the tone before the text. */
  dot?: boolean;
  children: ReactNode;
}

export function StatusPill({ tone, size = 22, dot = false, children }: StatusPillProps) {
  return (
    <span className="pill" data-tone={tone} data-size={size}>
      {dot && <span className="pill-dot" aria-hidden="true" />}
      {children}
    </span>
  );
}
