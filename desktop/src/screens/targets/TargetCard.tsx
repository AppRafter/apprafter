// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One target in the store: its name, the CLI-default tag, provider and region, default tier and
// server type as stored, and its two actions — open it (or switch to its tab) and make it the
// CLI's default. No health: nothing has measured it, and the app never shows "reachable"
// untested (health arrives with D.5).
import { useId } from 'react';
import { Button } from '../../components/Button';
import { Tag } from '../../components/Tag';
import type { TargetSummary } from '../../ipc/generated/TargetSummary';
import { serverLine, tierLabel } from './labels';

export interface TargetCardProps {
  readonly target: TargetSummary;
  /** It has a tab: the button switches to it instead of opening another. */
  readonly open: boolean;
  readonly onOpen: (name: string) => void;
  readonly onMakeDefault: (name: string) => void;
}

const tierLine = (target: TargetSummary) =>
  target.defaultTier === null && target.tierLevel === null
    ? 'Default tier not set'
    : tierLabel(target.defaultTier, target.tierLevel);

export function TargetCard({ target, open, onOpen, onMakeDefault }: TargetCardProps) {
  const nameId = useId();
  const { name } = target;
  return (
    <article className="target-card" aria-labelledby={nameId}>
      <span className="target-card-head">
        <span className="target-card-name" id={nameId}>
          {name}
        </span>
        {target.isCliDefault && (
          <Tag variant="outline" mono>
            CLI default
          </Tag>
        )}
      </span>
      <span className="target-card-meta">{`${target.provider} · ${target.region ?? 'region not set'}`}</span>
      <span className="target-card-meta">{serverLine(target.serverType)}</span>
      <span className="target-card-tier">{tierLine(target)}</span>
      <span className="target-card-actions">
        <Button
          size={28}
          variant={open ? 'secondary' : 'primary'}
          aria-label={`${open ? 'Switch to' : 'Open'} ${name}`}
          onClick={() => onOpen(name)}
        >
          {open ? 'Switch to tab' : 'Open in new tab'}
        </Button>
        {!target.isCliDefault && (
          <Button
            size={28}
            variant="ghost"
            aria-label={`Make ${name} the CLI default`}
            onClick={() => onMakeDefault(name)}
          >
            Make default for the CLI
          </Button>
        )}
      </span>
    </article>
  );
}
