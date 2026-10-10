// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Target section of a tab: what `target show` reports, read from the store every
// STORE_REFRESH_MS while the tab is shown, and what can be done to the target here — rename,
// renew the token, change the SSH key, make it the CLI's default, remove it from this computer —
// each by its plan class (actions.tsx). A target whose own file cannot be read shows why, and its
// Danger zone still removes it (WI-458): the Targets page lists by config.yaml alone, so a target
// whose credentials do not parse opens here. A plan refused, or a run that failed or was cancelled,
// shows above the cards with what it offers; an action that starts again clears it. Run doctor
// (the page header) and the Machine row's Change open the D.3 flows (screens/flows.tsx): Doctor
// over every view, Change machine in this tab, on the machine the report says the target is set
// to.
import { useCallback, useState } from 'react';
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { SpinnerGapIcon, StethoscopeIcon } from '../../components/icons';
import { PageGrid } from '../../components/PageGrid';
import { PageHeader } from '../../components/PageHeader';
import { StatePanel } from '../../components/StatePanel';
import { uiErrorOf } from '../../ipc/api';
import { CORE_ERROR_CODES, type ErrorAction, errorAction } from '../../ipc/errors';
import type { UiError } from '../../ipc/generated/UiError';
import { sectionInfo } from '../../shell/sections';
import { usePlatform } from '../../state/platform';
import { useTargetReport } from '../../state/targets';
import { useTargetFlows } from '../flows';
import { useTargetActions } from './actions';
import { DangerZone } from './DangerZone';
import { TargetDetails } from './TargetDetails';

export interface TargetScreenProps {
  readonly name: string;
  /** A rename ran: the tab follows the new name. */
  readonly onRenamed: (from: string, to: string) => void;
  /** A remove ran: the tab closes. */
  readonly onRemoved: (name: string) => void;
  /** Opens the machine picker; by default the D.3 flow's, on the machine the report names. */
  readonly onChangeMachine?: () => void;
}

export function TargetScreen({ name, onRenamed, onRemoved, onChangeMachine }: TargetScreenProps) {
  const info = usePlatform();
  const report = useTargetReport(name);
  const flows = useTargetFlows();
  const loaded = report.data;
  // The machine the target is set to, as its report says: where the picker opens.
  const changeMachine =
    onChangeMachine ??
    (loaded === undefined
      ? undefined
      : () =>
          flows.changeMachine(loaded.name, {
            region: loaded.region,
            serverType: loaded.serverType,
          }));
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

  // The ErrorPanel's actions this screen runs, and those the D.3 flows own; any other is not
  // offered here.
  const runs = (action: ErrorAction): (() => void) | null => {
    if (action.kind === 'renew-token') return actions.renew;
    if (action.kind === 'machine-picker' && changeMachine !== undefined) return changeMachine;
    if (action.kind === 'toolchain' || action.kind === 'add-target') {
      return () => flows.errorAction(action);
    }
    return null;
  };
  const failureRun = failure === null ? null : runs(errorAction(failure));

  return (
    <div className="page">
      <PageHeader
        title="Target"
        sub={sectionInfo('target').sub?.(name)}
        actions={
          <Button variant="primary" icon={StethoscopeIcon} onClick={() => flows.doctor(name)}>
            Run doctor
          </Button>
        }
      />
      {failure !== null && (
        <ErrorPanel
          error={failure}
          {...(failureRun !== null && { onAction: () => fresh(failureRun)() })}
        />
      )}
      {report.isPending ? (
        <StatePanel icon={SpinnerGapIcon} spin title="Reading the target…" />
      ) : report.isError ? (
        <Unreadable error={uiErrorOf(report.error)} name={name} onRemove={fresh(actions.remove)} />
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
                onChangeMachine={changeMachine === undefined ? null : fresh(changeMachine)}
              />
              <DangerZone onRemove={fresh(actions.remove)} />
            </>
          }
        />
      )}
    </div>
  );
}

/**
 * Why the target cannot be shown; when it is one of its own files (`fields.target` names it, as
 * the core projects it), the Danger zone too: removing it works on a target that cannot be read
 * (WI-458), and the error's help offers it. Never for the store's own config.yaml, which no
 * removal repairs.
 */
function Unreadable({
  error,
  name,
  onRemove,
}: {
  error: UiError;
  name: string;
  onRemove: () => void;
}) {
  const ownFile =
    error.code === CORE_ERROR_CODES.TARGET_INVALID_CONFIG && error.fields.target === name;
  return (
    <>
      <ErrorPanel error={error} />
      {ownFile && <DangerZone onRemove={onRemove} />}
    </>
  );
}
