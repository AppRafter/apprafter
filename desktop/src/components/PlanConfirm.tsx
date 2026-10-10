// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The confirm for a plan Rust holds (spec §4.4, decision 3): a bounded plan is a plain confirm
// that lists its changes; a destructive one shows the plan, the type-the-name guard where it is
// given, and Rust asks the OS gesture inside op_execute. A reversible plan has no dialog: the
// page runs it at once. Confirmed, the dialog stays busy until the run ends, then closes and
// hands over the result or the failure (a cancelled run is a failure with OP_CANCELLED). Closed
// before it ran, it discards the plan in Rust, and with it whatever the plan holds (a renew
// plan's token); a plan its tab held (heldPlans) is released either way, so the tab's closing
// does not discard it again. Gone while its plan runs (a lock, a closed tab: Rust runs a confirmed
// plan on), it leaves the end to the app, which shows it and only then discards it (ipc/away.ts):
// held covers a plan not yet started, away one that started, so no plan is discarded twice.
import { type ReactNode, useEffect, useRef } from 'react';
import * as api from '../ipc/api';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import type { PlanView } from '../ipc/generated/PlanView';
import type { JsonValue } from '../ipc/generated/serde_json/JsonValue';
import type { UiError } from '../ipc/generated/UiError';
import { releasePlan } from '../ipc/heldPlans';
import { OperationFailed, resultOf, startPlan } from '../ipc/plans';
import { ConfirmDialog } from './ConfirmDialog';
import type { Icon } from './icons';
import { PlanChanges } from './PlanChanges';

export interface PlanConfirmProps {
  /** A bounded or destructive plan; a reversible one runs without a dialog. */
  readonly view: PlanView;
  readonly title: string;
  readonly body?: ReactNode;
  readonly confirmLabel: string;
  /** Destructive: the text to type first (the target's name). */
  readonly requireText?: string;
  readonly icon?: Icon;
  readonly auth: AuthInfo | null;
  readonly onDone: (result: JsonValue) => void;
  /** It ran and failed, or was cancelled: the dialog closes, and the caller shows this. */
  readonly onFailed: (error: UiError) => void;
  readonly onClose: () => void;
}

export function PlanConfirm({
  view,
  title,
  body,
  confirmLabel,
  requireText,
  icon,
  auth,
  onDone,
  onFailed,
  onClose,
}: PlanConfirmProps) {
  const started = useRef(false);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const planClass = view.class;
  if (planClass === 'reversible') {
    throw new Error(`plan ${view.opId} is reversible: run it without a confirm`);
  }
  const destructive = planClass === 'destructive';
  const changes = <PlanChanges changes={view.changes} />;

  const confirm = async (password?: string) => {
    const run = await startPlan(view.opId, password, {
      title: view.title,
      shown: () => alive.current,
    });
    started.current = true;
    releasePlan(view.opId);
    const end = await run.ended;
    if (!alive.current) return;
    let result: JsonValue;
    try {
      result = resultOf(end);
    } catch (e) {
      if (e instanceof OperationFailed) {
        onFailed(e.error);
        return;
      }
      throw e;
    }
    onDone(result);
  };

  const close = () => {
    if (!started.current) {
      releasePlan(view.opId);
      api.opDiscard(view.opId).catch((e: unknown) => {
        console.error(`op_discard ${view.opId} failed:`, e);
      });
    }
    onClose();
  };

  return (
    <ConfirmDialog
      title={title}
      body={
        destructive ? (
          body
        ) : (
          <>
            {body}
            {changes}
          </>
        )
      }
      confirmLabel={confirmLabel}
      danger={destructive}
      planClass={planClass}
      {...(destructive && { plan: changes })}
      {...(requireText !== undefined && { requireText })}
      {...(icon !== undefined && { icon })}
      auth={auth}
      onConfirm={confirm}
      onClose={close}
    />
  );
}
