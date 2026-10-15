// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';

export interface PageHeaderProps {
  title: string;
  sub?: ReactNode;
  /** Buttons at the right of the title (the Target screen's Run doctor). */
  actions?: ReactNode;
  /** 22: the larger title of a screen's own page. */
  size?: 20 | 22;
}

/**
 * A page's title and sub, with an optional actions slot beside them. The title can take the
 * focus (not by Tab): where a dialog sends it back when the control that opened it is gone.
 */
export function PageHeader({ title, sub, actions, size }: PageHeaderProps) {
  return (
    <header className="page-header" data-actions={actions === undefined ? undefined : true}>
      <div className="page-titles">
        <h1 className="page-title" data-size={size} tabIndex={-1}>
          {title}
        </h1>
        {sub !== undefined && <p className="page-sub">{sub}</p>}
      </div>
      {actions !== undefined && <div className="page-actions">{actions}</div>}
    </header>
  );
}
