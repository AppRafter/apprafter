// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Targets view: every target in the local target store (target_list), each opening in its
// own tab; an unreadable one gets a card saying why; a CLI default that names no target says so.
// Make default for the CLI runs the reversible use plan at once.
import { useCallback, useState } from 'react';
import { ErrorPanel } from '../../components/ErrorPanel';
import { SpinnerGapIcon } from '../../components/icons';
import { PageHeader } from '../../components/PageHeader';
import { StatePanel } from '../../components/StatePanel';
import { uiErrorOf } from '../../ipc/api';
import type { TargetListReport } from '../../ipc/generated/TargetListReport';
import type { UiError } from '../../ipc/generated/UiError';
import { useTargetList } from '../../state/targets';
import { useMakeDefault } from '../target/actions';
import { AddTargetCard } from './AddTargetCard';
import { TargetCard } from './TargetCard';
import { UnreadableCard } from './UnreadableCard';

export interface TargetsPageProps {
  readonly onOpen: (name: string) => void;
  /** The targets with a tab: their cards switch to it. */
  readonly openTargets: ReadonlySet<string>;
}

export function TargetsPage({ onOpen, openTargets }: TargetsPageProps) {
  const list = useTargetList();
  const [failure, setFailure] = useState<UiError | null>(null);
  const makeDefault = useMakeDefault(setFailure);
  const onMakeDefault = useCallback(
    (name: string) => {
      setFailure(null);
      void makeDefault(name);
    },
    [makeDefault],
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
}: {
  report: TargetListReport;
  openTargets: ReadonlySet<string>;
  onOpen: (name: string) => void;
  onMakeDefault: (name: string) => void;
}) {
  const empty = report.targets.length === 0 && report.unreadable.length === 0;
  return (
    <>
      {report.cliDefault.status === 'missing' && (
        <p className="page-notice">
          {`The CLI default points at ${report.cliDefault.name}, which is not in the store.`}
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
          <UnreadableCard key={target.name} target={target} />
        ))}
        <AddTargetCard />
      </div>
    </>
  );
}
