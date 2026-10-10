// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';
import type { Tone } from './tone';

export interface TagProps {
  tone?: Tone;
  /** tinted: plan class, doctor result; outline: scope, phase; chip: envs, keys, targets. */
  variant?: 'tinted' | 'outline' | 'chip';
  mono?: boolean;
  children: ReactNode;
}

export function Tag({ tone = 'neutral', variant = 'tinted', mono = false, children }: TagProps) {
  return (
    <span className="tag" data-tone={tone} data-variant={variant} data-mono={mono || undefined}>
      {children}
    </span>
  );
}
