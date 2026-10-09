// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';
import { Modal } from './Modal';

export interface InfoSheetProps {
  title: string;
  sub?: ReactNode;
  rows: readonly { readonly k: string; readonly v: ReactNode }[];
  onClose: () => void;
}

/** Read-only facts as a term list (snapshot Inspect, the local-state preview). */
export function InfoSheet({ title, sub, rows, onClose }: InfoSheetProps) {
  return (
    <Modal title={title} sub={sub} width={540} layer="form" onClose={onClose} dismissOnBackdrop>
      <dl className="kv">
        {rows.map((row) => (
          <div className="kv-row" key={row.k}>
            <dt>{row.k}</dt>
            <dd>{row.v}</dd>
          </div>
        ))}
      </dl>
    </Modal>
  );
}
