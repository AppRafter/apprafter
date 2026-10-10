// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The plans a view's confirm holds before they run, by the tab that opened them, so a tab that
// closes discards them in Rust with whatever they hold — a renew plan's token (D.3d review #13).
// A confirm closed before it ran discards its own plan (PlanConfirm); a run that started is
// released. The tab's closing is told by the Shell, never by an effect cleanup: a hidden tab's
// Activity runs those, and a confirm hidden with its tab keeps its plan for when it is back.
// Plan expiry in Rust stays the backstop.
import * as api from './api';
import type { OpId } from './generated/OpId';

/** Each held plan's tab, by its op id. */
const held = new Map<OpId, string>();
/** Tabs that closed: a plan handed to one later (made while it closed) goes at once. */
const closed = new Set<string>();

const discard = (opId: OpId) => {
  api.opDiscard(opId).catch((e: unknown) => {
    console.error(`op_discard ${opId} failed:`, e);
  });
};

/** The confirm of the tab `owner` holds plan `opId`, not yet run. */
export function holdPlan(owner: string, opId: OpId): void {
  if (closed.has(owner)) {
    discard(opId);
    return;
  }
  held.set(opId, owner);
}

/** Plan `opId` started, or its confirm discarded it: nobody holds it any more. */
export function releasePlan(opId: OpId): void {
  held.delete(opId);
}

/** The tab `owner` closed: every plan its confirms hold is discarded, and any handed to it later. */
export function discardPlansOf(owner: string): void {
  closed.add(owner);
  for (const [opId, by] of [...held]) {
    if (by !== owner) continue;
    held.delete(opId);
    discard(opId);
  }
}

/** Tests start from nothing held. */
export function resetHeldPlans(): void {
  held.clear();
  closed.clear();
}
