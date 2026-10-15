// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// When a screen is gone (D.3e review 17-20, the rule): only after one of three events, each a
// scope that ends. The lock ends the session's scope (LockGate; Rust also cancels the reads,
// drops the drafts and the plans not yet run). A tab closing ends its view's scope (the Shell,
// when it removes the tab). A dialog its owner closes ends its overlay's scope (ViewFrame's close,
// the app's own overlays, Settings). A scope ends with its parent: an overlay goes with its view,
// everything with the lock. Never an effect cleanup: a view <Activity> hides runs its cleanups
// without going, and what it started keeps running for when it is shown again.
//
// What a screen started registers with its scope when it starts (a read, a plan its confirm
// holds, the wizard's draft) and is cancelled or discarded when the scope ends.

export interface Scope {
  /** Whether the screen this scope stands for is gone. */
  readonly gone: () => boolean;
  /** Runs `onEnd` when the scope ends, at once if it has; returns the unregister. */
  readonly onGone: (onEnd: () => void) => () => void;
}

export interface ScopeHandle {
  readonly scope: Scope;
  /** The screen went: everything registered with it runs, once (its children end with it). */
  readonly end: () => void;
}

const NOTHING = () => {};

/** A scope that ends when `end` is called, or with `parent`. */
export function newScope(parent: Scope | null): ScopeHandle {
  let ended = false;
  const waiting = new Set<() => void>();
  let offParent = NOTHING;
  const end = () => {
    if (ended) return;
    ended = true;
    offParent();
    const all = [...waiting];
    waiting.clear();
    for (const onEnd of all) onEnd();
  };
  // A parent that is gone already ends this one at once.
  if (parent !== null) offParent = parent.onGone(end);
  const scope: Scope = {
    gone: () => ended,
    onGone: (onEnd) => {
      if (ended) {
        onEnd();
        return NOTHING;
      }
      // A wrapper per registration: the same function registered twice is two registrations.
      const once = () => onEnd();
      waiting.add(once);
      return () => {
        waiting.delete(once);
      };
    },
  };
  return { scope, end };
}

let session = newScope(null);

/** The scope of the unlocked session: it ends on the next lock. */
export function sessionScope(): Scope {
  return session.scope;
}

/** The app locked: every screen of the session is gone. The next session starts now. */
export function sessionLocked(): void {
  const was = session;
  session = newScope(null);
  was.end();
}

/** Tests start from a session nothing has ended. */
export function resetLifecycle(): void {
  session = newScope(null);
}
