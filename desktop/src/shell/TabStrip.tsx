// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The open targets as tabs (brief §2): 184px each, health dot (unknown until the notifier
// measures it, D.5), name, approvals badge (hidden at zero), close; and "+" for the Targets
// view. A spinner replaces the dot while an operation runs on that target.
import { Badge } from '../components/Badge';
import { Dot } from '../components/Dot';
import { IconButton } from '../components/IconButton';
import { PlusIcon, SpinnerGapIcon, XIcon } from '../components/icons';
import type { Os } from '../ipc/generated/Os';
import type { TargetTab, View } from '../state/session';
import { shortcutHint } from '../state/shortcuts';

export interface TabStripProps {
  tabs: readonly TargetTab[];
  view: View;
  os: Os;
  /** Targets with an operation running. */
  running: ReadonlySet<string>;
  /** Approvals waiting per target (the notifier, D.5); none until then. */
  approvals?: (target: string) => number;
  onShow: (view: View) => void;
  onClose: (key: string) => void;
  onNewTab: () => void;
}

export function TabStrip({
  tabs,
  view,
  os,
  running,
  approvals = () => 0,
  onShow,
  onClose,
  onNewTab,
}: TabStripProps) {
  return (
    <div className="tabstrip">
      <div className="tabs" role="tablist" aria-label="Open clusters">
        {tabs.map((tab) => {
          const selected = view.kind === 'tab' && view.key === tab.key;
          return (
            <div key={tab.key} className="tab" data-selected={selected || undefined}>
              <button
                type="button"
                role="tab"
                aria-selected={selected}
                className="tab-main"
                onClick={() => onShow({ kind: 'tab', key: tab.key })}
              >
                {running.has(tab.target) ? (
                  <span className="tab-status" role="img" aria-label="An operation is running">
                    <SpinnerGapIcon className="spin" aria-hidden="true" />
                  </span>
                ) : (
                  <Dot />
                )}
                <span className="tab-name">{tab.target}</span>
                <Badge count={approvals(tab.target)} label="approvals waiting" />
              </button>
              <IconButton
                label={`Close ${tab.target}`}
                icon={XIcon}
                size={22}
                onClick={(event) => {
                  event.stopPropagation();
                  onClose(tab.key);
                }}
              />
            </div>
          );
        })}
      </div>
      <IconButton
        label="Open a cluster"
        title={`Open a cluster (${shortcutHint('targets', os)})`}
        icon={PlusIcon}
        size={30}
        pressed={view.kind === 'targets'}
        onClick={onNewTab}
      />
    </div>
  );
}
