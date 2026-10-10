// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Target section of a tab: what `target show` reports, read from the store every
// STORE_REFRESH_MS while the tab is shown, and what can be done to the target here — rename,
// renew the token, change the SSH key, make it the CLI's default, remove it from this computer —
// each by its plan class (actions.tsx). A plan refused, or a run that failed or was cancelled,
// shows above the cards with what it offers; an action that starts again clears it.
import { useCallback, useState } from 'react';
import { ErrorPanel } from '../../components/ErrorPanel';
import { SpinnerGapIcon } from '../../components/icons';
import { PageGrid } from '../../components/PageGrid';
import { PageHeader } from '../../components/PageHeader';
import { StatePanel } from '../../components/StatePanel';
import { uiErrorOf } from '../../ipc/api';
import { type ErrorAction, errorAction } from '../../ipc/errors';
import type { UiError } from '../../ipc/generated/UiError';
import { sectionInfo } from '../../shell/sections';
import { usePlatform } from '../../state/platform';
import { useTargetReport } from '../../state/targets';
import { useTargetActions } from './actions';
import { DangerZone } from './DangerZone';
import { TargetDetails } from './TargetDetails';

export interface TargetScreenProps {
  readonly name: string;
  /** A rename ran: the tab follows the new name. */
  readonly onRenamed: (from: string, to: string) => void;
  /** A remove ran: the tab closes. */
  readonly onRemoved: (name: string) => void;
  /** Opens the machine picker (D.3e); without it the Machine row offers no Change. */
  readonly onChangeMachine?: () => void;
}

export function TargetScreen({ name, onRenamed, onRemoved, onChangeMachine }: TargetScreenProps) {
  const info = usePlatform();
  const report = useTargetReport(name);
  const [failure, setFailure] = useState<UiError | null>(null);
  const actions = useTargetActions({ name, onRenamed, onRemoved, onFailed: setFailure });
  /** An action starting again: what failed last is not the news any more. */
  const fresh = useCallback(
    (run: () => void) => () => {
      setFailure(null);
      run();
    },
    [],
  );

  // The ErrorPanel's actions this screen runs; any other is not offered here.
  const runs = (action: ErrorAction): (() => void) | null => {
    if (action.kind === 'renew-token') return actions.renew;
    if (action.kind === 'machine-picker' && onChangeMachine !== undefined) return onChangeMachine;
    return null;
  };
  const failureRun = failure === null ? null : runs(errorAction(failure));

  return (
    <div className="page">
      <PageHeader title="Target" sub={sectionInfo('target').sub?.(name)} />
      {failure !== null && (
        <ErrorPanel
          error={failure}
          {...(failureRun !== null && { onAction: () => fresh(failureRun)() })}
        />
      )}
      {report.isPending ? (
        <StatePanel icon={SpinnerGapIcon} spin title="Reading the target…" />
      ) : report.isError ? (
        <ErrorPanel error={uiErrorOf(report.error)} />
      ) : (
        // The design's side column holds Cluster access (D.11) above the Danger zone. Until that
        // card exists the Danger zone follows the Target card in one column, and the card keeps
        // the page's width (plan deviation 8).
        <PageGrid
          main={
            <>
              <TargetDetails
                report={report.data}
                os={info.os}
                secretBackend={info.secretBackend}
                actions={{
                  rename: fresh(actions.rename),
                  renew: fresh(actions.renew),
                  makeDefault: fresh(actions.makeDefault),
                  changeSshKey: fresh(actions.changeSshKey),
                }}
                onChangeMachine={onChangeMachine === undefined ? null : fresh(onChangeMachine)}
              />
              <DangerZone onRemove={fresh(actions.remove)} />
            </>
          }
        />
      )}
    </div>
  );
}
