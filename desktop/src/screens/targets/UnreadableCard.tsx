// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A target whose files cannot be read: listed with what the store said, never hidden (R7: the
// CLI only logs it), tagged when the CLI default names it. It offers nothing to open: there is
// no report to show.
import { useId } from 'react';
import { Tag } from '../../components/Tag';
import type { UnreadableTarget } from '../../ipc/generated/UnreadableTarget';

export interface UnreadableCardProps {
  readonly target: UnreadableTarget;
  /** The CLI's default names this target. */
  readonly isCliDefault?: boolean;
}

export function UnreadableCard({ target, isCliDefault = false }: UnreadableCardProps) {
  const id = useId();
  return (
    <article className="target-card" data-state="unreadable" aria-labelledby={id}>
      <span className="target-card-head">
        <span className="target-card-name" id={id}>
          {target.name}
        </span>
        {isCliDefault && (
          <Tag variant="outline" mono>
            CLI default
          </Tag>
        )}
        <Tag tone="err">Cannot be read</Tag>
      </span>
      <p className="target-card-error">{target.error.message}</p>
      {target.error.code !== null && <span className="target-card-meta">{target.error.code}</span>}
    </article>
  );
}
