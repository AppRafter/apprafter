// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Targets view: every target in the local target store (target_list), each opening in its
// own tab; one whose config.yaml cannot be read gets a card saying why, and can be removed
// (WI-458: the same destructive plan as the Target screen's); a CLI default that names no target,
// or one that cannot be read, says so. Make default for the CLI runs the reversible use plan at
// once; the core refuses a target whose files cannot be read (WI-458 review #4), shown here.
import { useCallback, useState } from 'react';
import { ErrorPanel } from '../../components/ErrorPanel';
import { SpinnerGapIcon } from '../../components/icons';
import { PageHeader } from '../../components/PageHeader';
import { StatePanel } from '../../components/StatePanel';
import { uiErrorOf } from '../../ipc/api';
import type { TargetListReport } from '../../ipc/generated/TargetListReport';
import type { UiError } from '../../ipc/generated/UiError';
import { useTargetList } from '../../state/targets';
import { useMakeDefault, useRemoveTarget } from '../target/actions';
import { AddTargetCard } from './AddTargetCard';
import { TargetCard } from './TargetCard';
import { UnreadableCard } from './UnreadableCard';

export interface TargetsPageProps {
  readonly onOpen: (name: string) => void;
  /** The targets with a tab: their cards switch to it. */
  readonly openTargets: ReadonlySet<string>;
  /** A target was removed here: a tab still open on it closes. */
  readonly onRemoved: (name: string) => void;
}

export function TargetsPage({ onOpen, openTargets, onRemoved }: TargetsPageProps) {
  const list = useTargetList();
  const [failure, setFailure] = useState<UiError | null>(null);
  const makeDefault = useMakeDefault(setFailure);
  // The view is not the target's: its tabs close the moment the removal ends (review #5).
  const removeTarget = useRemoveTarget(onRemoved, setFailure, { closeAtOnce: true });
  const onMakeDefault = useCallback(
    (name: string) => {
      setFailure(null);
      void makeDefault(name);
    },
    [makeDefault],
  );
  const onRemove = useCallback(
    (name: string) => {
      setFailure(null);
      void removeTarget(name);
    },
    [removeTarget],
  );
  return (
    <div className="page">
      <PageHeader
        title="Open a cluster"
        size={22}
        sub="Targets live in your local target store. Each opens in its own tab."
      />
      {failure !== null && <ErrorPanel error={failure} />}
      {list.isPending ? (
        <StatePanel icon={SpinnerGapIcon} spin title="Reading the target store…" />
      ) : list.isError ? (
        <ErrorPanel error={uiErrorOf(list.error)} />
      ) : (
        <Store
          report={list.data}
          openTargets={openTargets}
          onOpen={onOpen}
          onMakeDefault={onMakeDefault}
          onRemove={onRemove}
        />
      )}
    </div>
  );
}

function Store({
  report,
  openTargets,
  onOpen,
  onMakeDefault,
  onRemove,
}: {
  report: TargetListReport;
  openTargets: ReadonlySet<string>;
  onOpen: (name: string) => void;
  onMakeDefault: (name: string) => void;
  onRemove: (name: string) => void;
}) {
  const empty = report.targets.length === 0 && report.unreadable.length === 0;
  const pointer = report.cliDefault;
  const cliDefault = pointer.status === 'unset' ? null : pointer.name;
  const unreadableDefault =
    pointer.status === 'set' && report.unreadable.some((target) => target.name === pointer.name);
  return (
    <>
      {pointer.status === 'missing' && (
        <p className="page-notice">
          {`The CLI default points at ${pointer.name}, which is not in the store.`}
        </p>
      )}
      {unreadableDefault && (
        <p className="page-notice">
          {`The CLI default points at ${cliDefault}, which cannot be read: CLI commands that name no target fail until it is fixed.`}
        </p>
      )}
      {empty && (
        <StatePanel
          title="No targets yet"
          text="Add one here, or in a terminal:"
          meta="apprafter target add"
        />
      )}
      <div className="target-grid">
        {report.targets.map((target) => (
          <TargetCard
            key={target.name}
            target={target}
            open={openTargets.has(target.name)}
            onOpen={onOpen}
            onMakeDefault={onMakeDefault}
          />
        ))}
        {report.unreadable.map((target) => (
          <UnreadableCard
            key={target.name}
            target={target}
            isCliDefault={target.name === cliDefault}
            onRemove={onRemove}
          />
        ))}
        <AddTargetCard />
      </div>
    </>
  );
}
