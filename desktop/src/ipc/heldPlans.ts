// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The plans a confirm holds before they run, by the scope of the screen that opened it
// (ipc/lifecycle.ts), so a screen that goes discards them in Rust with whatever they hold — a
// renew plan's token (D.3d review #13): its tab closed, the lock (Rust drops them too; the discard
// is then refused as locked, which is expected). A confirm closed before it ran discards its own
// plan (PlanConfirm); a run that started is released. Never an effect cleanup: a confirm hidden
// with its tab keeps its plan for when it is back. Plan expiry in Rust stays the backstop.
import * as api from './api';
import type { OpId } from './generated/OpId';
import type { Scope } from './lifecycle';
import { reportUnlessLocked } from './plans';

/** Each held plan's unregister from its scope, by its op id. */
const held = new Map<OpId, () => void>();

const discard = (opId: OpId) => {
  api.opDiscard(opId).catch(reportUnlessLocked(`op_discard ${opId}`));
};

/** A confirm of the screen `scope` stands for holds plan `opId`, not yet run. */
export function holdPlan(scope: Scope, opId: OpId): void {
  // A screen gone already (a plan made after its tab closed): the plan goes at once.
  const off = scope.onGone(() => {
    held.delete(opId);
    discard(opId);
  });
  if (!scope.gone()) held.set(opId, off);
}

/** Plan `opId` started, or its confirm discarded it: nobody holds it any more. */
export function releasePlan(opId: OpId): void {
  held.get(opId)?.();
  held.delete(opId);
}

/** Tests start from nothing held. */
export function resetHeldPlans(): void {
  held.clear();
}
