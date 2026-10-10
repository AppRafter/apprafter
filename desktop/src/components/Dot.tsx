// SPDX-License-Identifier: FSL-1.1-Apache-2.0

export interface DotProps {
  /** `unknown` until something has been measured: the app never shows "reachable" untested. */
  tone?: 'ok' | 'warn' | 'err' | 'unknown' | 'accent';
  size?: 6 | 7 | 8;
  /** Names the state for a screen reader; without it the dot is decoration. */
  label?: string;
}

export function Dot({ tone = 'unknown', size = 7, label }: DotProps) {
  return label === undefined ? (
    <span className="dot" data-tone={tone} data-size={size} aria-hidden="true" />
  ) : (
    <span className="dot" data-tone={tone} data-size={size} role="img" aria-label={label} />
  );
}
