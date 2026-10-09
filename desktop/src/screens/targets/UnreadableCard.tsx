// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A target whose files cannot be read: listed with what the store said, never hidden (R7: the
// CLI only logs it). It offers nothing to open: there is no report to show.
import { useId } from 'react';
import { Tag } from '../../components/Tag';
import type { UnreadableTarget } from '../../ipc/generated/UnreadableTarget';

export function UnreadableCard({ target }: { target: UnreadableTarget }) {
  const id = useId();
  return (
    <article className="target-card" data-state="unreadable" aria-labelledby={id}>
      <span className="target-card-head">
        <span className="target-card-name" id={id}>
          {target.name}
        </span>
        <Tag tone="err">Cannot be read</Tag>
      </span>
      <p className="target-card-error">{target.error.message}</p>
      {target.error.code !== null && <span className="target-card-meta">{target.error.code}</span>}
    </article>
  );
}
