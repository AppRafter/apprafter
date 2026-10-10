// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A target whose config.yaml cannot be read (the list reads no other file): listed with what the
// store said, never hidden (R7: the CLI only logs it), tagged when the CLI default names it. It offers nothing to open — there is
// no report to show — and one action: remove it from this computer (WI-458), through the same
// destructive plan as the Target screen's (its lines name the file that cannot be read).
import { useId } from 'react';
import { Button } from '../../components/Button';
import { TrashIcon } from '../../components/icons';
import { Tag } from '../../components/Tag';
import type { UnreadableTarget } from '../../ipc/generated/UnreadableTarget';

export interface UnreadableCardProps {
  readonly target: UnreadableTarget;
  /** The CLI's default names this target. */
  readonly isCliDefault?: boolean;
  /** Remove it: the plan, its confirm and the gesture (actions.tsx's useRemoveTarget). */
  readonly onRemove: (name: string) => void;
}

export function UnreadableCard({ target, isCliDefault = false, onRemove }: UnreadableCardProps) {
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
      <span className="target-card-actions">
        <Button
          size={28}
          variant="danger"
          icon={TrashIcon}
          aria-label={`Remove ${target.name}`}
          onClick={() => onRemove(target.name)}
        >
          Remove…
        </Button>
      </span>
    </article>
  );
}
