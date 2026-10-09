// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';
import type { Icon } from './icons';

export interface StatePanelProps {
  icon?: Icon;
  tone?: 'ok' | 'warn' | 'err' | 'neutral' | 'accent';
  /** The icon turns (provisioning, loading). */
  spin?: boolean;
  title: string;
  text?: ReactNode;
  /** Mono, faint: an address, a duration, a slice name. */
  meta?: ReactNode;
  /** Buttons: Retry, Run doctor… */
  actions?: ReactNode;
}

/** What a screen shows instead of its content: empty, unreachable, planned, provisioning. */
export function StatePanel({
  icon: StateIcon,
  tone = 'neutral',
  spin = false,
  title,
  text,
  meta,
  actions,
}: StatePanelProps) {
  return (
    <div className="state-panel" data-tone={tone}>
      {StateIcon && (
        <StateIcon className={spin ? 'state-icon spin' : 'state-icon'} aria-hidden="true" />
      )}
      <h2 className="state-title">{title}</h2>
      {text !== undefined && <div className="state-text">{text}</div>}
      {meta !== undefined && <div className="state-meta">{meta}</div>}
      {actions !== undefined && <div className="state-actions">{actions}</div>}
    </div>
  );
}
