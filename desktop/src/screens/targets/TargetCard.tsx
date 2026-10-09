// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One target: name, provider and region, tier, the CLI-default marker. No health: nothing has
// measured it, and the app never shows "reachable" untested (health arrives with D.5).
import { Tag } from '../../components/Tag';
import { type TargetSummary, tierLabel } from './targets';

export function TargetCard({
  target,
  onOpen,
}: {
  target: TargetSummary;
  onOpen: (name: string) => void;
}) {
  return (
    <button type="button" className="target-card" onClick={() => onOpen(target.name)}>
      <span className="target-card-head">
        <span className="target-card-name">{target.name}</span>
        {target.cliDefault && (
          <Tag variant="outline" mono>
            CLI default
          </Tag>
        )}
      </span>
      <span className="target-card-meta">{`${target.provider} · ${target.region}`}</span>
      <span className="target-card-tier">{tierLabel(target.tier)}</span>
    </button>
  );
}
