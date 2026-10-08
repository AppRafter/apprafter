// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';

export type EyebrowProps = { children: ReactNode; id?: string } & (
  | { as?: 'div' | 'span' }
  | { as: 'label'; htmlFor: string }
);

/** The design's most repeated label: mono 500 11px, 0.1em, uppercase, --fg-faint. */
export function Eyebrow(props: EyebrowProps) {
  if (props.as === 'label') {
    return (
      <label className="eyebrow" id={props.id} htmlFor={props.htmlFor}>
        {props.children}
      </label>
    );
  }
  const Tag = props.as ?? 'div';
  return (
    <Tag className="eyebrow" id={props.id}>
      {props.children}
    </Tag>
  );
}
