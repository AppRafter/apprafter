// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';

export interface SettingRowProps {
  label: ReactNode;
  sub?: ReactNode;
  /** Mono text on the right, ellipsised (a card's server type, region…). */
  value?: ReactNode;
  /** A segmented control, a switch or buttons. */
  control?: ReactNode;
  /**
   * Dims the row. The control must be disabled too (pass `disabled` to it): a dimmed row that
   * still answers clicks is the design's bug, not the app's.
   */
  disabled?: boolean;
  tone?: 'default' | 'danger';
}

/** One row of a settings section (padding 10 0) or, as CardRow, of a card (padding 10 16). */
export function SettingRow(props: SettingRowProps) {
  return <Row {...props} layout="setting" />;
}

export function CardRow(props: SettingRowProps) {
  return <Row {...props} layout="card" />;
}

function Row({
  label,
  sub,
  value,
  control,
  disabled = false,
  tone = 'default',
  layout,
}: SettingRowProps & { layout: 'setting' | 'card' }) {
  return (
    <div
      className="row"
      data-layout={layout}
      data-tone={tone}
      data-disabled={disabled || undefined}
    >
      <div className="row-text">
        <div className="row-label">{label}</div>
        {sub !== undefined && <div className="row-sub">{sub}</div>}
      </div>
      {value !== undefined && <div className="row-value">{value}</div>}
      {control !== undefined && <div className="row-control">{control}</div>}
    </div>
  );
}
