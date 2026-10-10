// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The confirm for a plan Rust holds (spec §4.4, decision 3): a bounded plan is a plain confirm
// that lists its changes; a destructive one shows the plan, the type-the-name guard where it is
// given, and Rust asks the OS gesture inside op_execute. A reversible plan has no dialog: the
// page runs it at once. Confirmed, the dialog stays busy until the run ends, then closes and
// hands over the result or the failure (a cancelled run is a failure with OP_CANCELLED). Closed
// before it ran, it discards the plan in Rust, and with it whatever the plan holds (a renew
// plan's token); a plan its screen held (heldPlans) is released either way, so the screen's going
// does not discard it again. Its screen gone while its plan runs (its tab closed, the lock: Rust
// runs a confirmed plan on; ipc/lifecycle.ts), it leaves the end to the app, which shows it and
// only then discards it (ipc/away.ts): held covers a plan not yet started, away one that started,
// so no plan is discarded twice. Only hidden (its tab's <Activity>), it keeps the end and applies
// it when it is shown again. Its screen gone while Rust asks a destructive plan's gesture, a
// refusal discards the plan the prompt kept from the hold's discard.
import { type ReactNode, useRef } from 'react';
import * as api from '../ipc/api';
import { keepEndedAway } from '../ipc/away';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import type { PlanView } from '../ipc/generated/PlanView';
import type { JsonValue } from '../ipc/generated/serde_json/JsonValue';
import type { UiError } from '../ipc/generated/UiError';
import { releasePlan } from '../ipc/heldPlans';
import type { OpEnd } from '../ipc/operations';
import { awayText, OperationFailed, reportUnlessLocked, resultOf, startPlan } from '../ipc/plans';
import { useScope } from '../state/scope';
import { useWhenShown } from '../state/whenShown';
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
  const scope = useScope();
  const started = useRef(false);
  // The confirm's wait, released once the end is applied: then the dialog closes.
  const applied = useRef<(() => void) | null>(null);
  const whenShown = useWhenShown(
    (end: OpEnd) => {
      try {
        onDone(resultOf(end));
      } catch (e) {
        if (!(e instanceof OperationFailed)) throw e;
        onFailed(e.error);
      } finally {
        applied.current?.();
      }
    },
    (end: OpEnd) => keepEndedAway({ opId: view.opId, ...awayText(view.title, end) }),
  );
  const planClass = view.class;
  if (planClass === 'reversible') {
    throw new Error(`plan ${view.opId} is reversible: run it without a confirm`);
  }
  const destructive = planClass === 'destructive';
  const changes = <PlanChanges changes={view.changes} />;

  const confirm = async (password?: string) => {
    const run = await startPlan(view.opId, password, {
      title: view.title,
      shown: () => !scope.gone(),
    }).catch((reason: unknown) => {
      // Refused, its screen gone meanwhile: its tab closed while Rust asked the OS gesture (a
      // busy confirm's own Close, Cancel and Esc are off), or the lock. The hold's discard came
      // while the plan was in the prompt, which Rust leaves alone, and a refusal it may retry
      // puts the plan back to wait for a try that cannot come now: it goes here. A refusal that
      // spent the plan leaves nothing to discard; after a lock the discard is refused as locked.
      if (scope.gone()) {
        api.opDiscard(view.opId).catch(reportUnlessLocked(`op_discard ${view.opId}`));
      }
      throw reason;
    });
    started.current = true;
    releasePlan(view.opId);
    const end = await run.ended;
    // Its screen went: the app shows the end (startPlan kept it away).
    if (scope.gone()) return;
    await new Promise<void>((resolve) => {
      applied.current = resolve;
      whenShown(end);
    });
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
