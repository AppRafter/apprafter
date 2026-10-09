// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Targets view: every target in the local target store, each opening in its own tab.
import { PlugIcon } from '../../components/icons';
import { StatePanel } from '../../components/StatePanel';
import { AddTargetCard } from './AddTargetCard';
import { TargetCard } from './TargetCard';
import { useTargets } from './targets';

export function TargetsPage({ onOpen }: { onOpen: (name: string) => void }) {
  const source = useTargets();
  return (
    <div className="page">
      <header className="page-header">
        <h1 className="page-title" data-size="22">
          Open a cluster
        </h1>
        <p className="page-sub">
          Targets live in your local target store. Each opens in its own tab.
        </p>
      </header>
      {source.kind === 'unavailable' ? (
        <>
          <StatePanel
            icon={PlugIcon}
            title="No data source yet — target commands arrive in D.3"
            text="Until then the CLI lists them:"
            meta="apprafter target list"
          />
          <div className="target-grid">
            <AddTargetCard />
          </div>
        </>
      ) : (
        <div className="target-grid">
          {source.targets.map((target) => (
            <TargetCard key={target.name} target={target} onOpen={onOpen} />
          ))}
          <AddTargetCard />
        </div>
      )}
    </div>
  );
}
