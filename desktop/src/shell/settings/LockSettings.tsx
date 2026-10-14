// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings › Lock's containers (D.4a §3.14): the Lock section on what app_info and lock_info say,
// and its flows on the IPC calls. The flows open beside Settings — SettingsDialog renders
// <LockFlow> next to its own modal, under Settings' scope, outside every view's <Activity>
// (GOTCHA-158) — so a plan a flow holds goes when Settings' scope ends, and never on an effect's
// cleanup.
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef, useState } from 'react';
import { ErrorPanel } from '../../components/ErrorPanel';
import { Modal } from '../../components/Modal';
import { useToast } from '../../components/Toast';
import * as api from '../../ipc/api';
import { uiErrorOf } from '../../ipc/api';
import type { PlanView } from '../../ipc/generated/PlanView';
import type { Settings } from '../../ipc/generated/Settings';
import type { UiError } from '../../ipc/generated/UiError';
import { holdPlan } from '../../ipc/heldPlans';
import { useLockActions } from '../../state/lock';
import { rereadLock, useLockInfo } from '../../state/lockInfo';
import { type ProtectAction, STOP_TITLE, stopOutcome, stoppedSummary } from '../../state/lockText';
import { usePlatform } from '../../state/platform';
import { useScope } from '../../state/scope';
import { useSaveSettings } from '../../state/settings';
import { FinishProtectDialog } from './FinishProtectDialog';
import { LockSection } from './LockSection';
import { ProtectDialog } from './ProtectDialog';
import { StopProtectingDialog } from './StopProtectingDialog';

export interface LockSettingsProps {
  readonly settings: Settings;
  readonly onAction: (action: ProtectAction) => void;
}

/** The Lock section, on app_info and lock_info. */
export function LockSettings({ settings, onAction }: LockSettingsProps) {
  const info = usePlatform();
  const save = useSaveSettings();
  const { lock } = useLockActions();
  const report = useLockInfo();
  return (
    <LockSection
      settings={settings}
      os={info.os}
      auth={info.auth}
      sessionEvents={info.sessionEvents}
      mode={info.secrets.mode}
      protectedStore={info.secrets.protected}
      problem={info.secrets.problem}
      info={report.data ?? null}
      error={report.isError ? uiErrorOf(report.error) : null}
      onSave={(change) => save(change)}
      onLock={() => {
        lock().catch((error: unknown) => console.error('lock_now failed:', error));
      }}
      onAction={onAction}
    />
  );
}

export interface LockFlowProps {
  readonly action: ProtectAction;
  readonly onClose: () => void;
}

/**
 * The flow `action` opens: Protect (a store in files; or an interrupted Protect that wrote no
 * vault, started over — its recovery key void); Finish (an interrupted Protect with its vault:
 * both secrets); Stop protecting, or the finish of an interrupted one.
 */
export function LockFlow({ action, onClose }: LockFlowProps) {
  const info = usePlatform();
  const client = useQueryClient();
  const done = () => rereadLock(client);
  const { mode, protected: vault, store } = info.secrets;
  if (action === 'protect' || (action === 'finish' && mode === 'locking' && !vault)) {
    return (
      <ProtectDialog
        store={store}
        restart={action === 'finish'}
        plan={(password) => api.opPlanLockOn(password, null)}
        keySaved={api.opRecoveryKeySaved}
        confirmKey={api.opConfirmRecoveryKey}
        copyKey={api.copyRecoveryKey}
        onDone={done}
        onClose={onClose}
      />
    );
  }
  if (action === 'finish' && mode === 'locking') {
    return (
      <FinishProtectDialog
        store={store}
        auth={info.auth}
        plan={api.opPlanLockFinish}
        onDone={done}
        onClose={onClose}
      />
    );
  }
  return <StopFlow finishing={action === 'finish'} onDone={done} onClose={onClose} />;
}

/** Stop protecting: its plan asked once, held by Settings' scope, confirmed with the store's secret. */
function StopFlow({
  finishing,
  onDone,
  onClose,
}: {
  finishing: boolean;
  onDone: () => void;
  onClose: () => void;
}) {
  const scope = useScope();
  const info = usePlatform();
  const toast = useToast();
  const [view, setView] = useState<PlanView | null>(null);
  // The plan uses the shared age key (only for a cache, d4a-f Task 3): the dialog names it then.
  const [needsKey, setNeedsKey] = useState(false);
  const [error, setError] = useState<UiError | null>(null);
  // The run failed: the dialog's own close that follows leaves the failure shown.
  const failed = useRef(false);
  // One plan, however often the effect runs (StrictMode runs it twice).
  const asked = useRef(false);
  useEffect(() => {
    if (asked.current) return;
    asked.current = true;
    api.opPlanLockOff().then(
      ({ plan, needsKey: usesKey }) => {
        if (scope.gone()) {
          api.opDiscard(plan.opId).catch((e: unknown) => console.error('op_discard failed:', e));
          return;
        }
        holdPlan(scope, plan.opId);
        setNeedsKey(usesKey);
        setView(plan);
      },
      (reason: unknown) => setError(uiErrorOf(reason)),
    );
  }, [scope]);
  if (error !== null) {
    return (
      <Modal title={STOP_TITLE} width={520} onClose={onClose}>
        <ErrorPanel error={error} />
      </Modal>
    );
  }
  if (view === null) return null;
  return (
    <StopProtectingDialog
      view={view}
      needsKey={needsKey}
      finishing={finishing}
      auth={info.auth}
      onDone={(result) => {
        // What really happened: another run first, a back-out, a token kept, the key used (UI
        // verifier M3; d4a-f Task 3).
        toast({ message: stoppedSummary(stopOutcome(result)) });
        onDone();
      }}
      onFailed={(failure) => {
        failed.current = true;
        setError(failure);
        onDone();
      }}
      onClose={() => {
        // A run that failed shows why, in the dialog's place.
        if (!failed.current) onClose();
      }}
    />
  );
}
