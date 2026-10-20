// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What a screen shows in its place when an action did not work (WI-483): a refusal before
// anything ran, or a change's end — its run failed or was cancelled — in an ErrorPanel. The lock
// unmounts the screen with the shell (LockGate), and the panel with it. A change's failure still
// shown then is kept for after the unlock (ipc/away.ts), in a line under the plan's title with its
// code, as an end that arrives while the app is locked is kept, and the app shows it again then
// (D.4a N5 does the same for a change's toast). It is kept from the moment the screen is told,
// never a render later, so a lock that lands before the panel paints keeps it too. Not kept: a
// refusal (nothing changed, and the flow says it again when it is opened again); a failure the
// owner cleared, or closed with its screen (its dialog, its tab: that scope ended first); one its
// tab hides when the lock comes — a hidden view runs its effects' cleanups (GOTCHA-158), and it
// waits again once shown — and one told to a screen that is gone already, whose run's end the app
// kept itself (startPlan, useWhenShown).
import { useCallback, useEffect, useRef, useState } from 'react';
import { keepEndedAway } from '../ipc/away';
import type { UiError } from '../ipc/generated/UiError';
import { sessionScope } from '../ipc/lifecycle';
import { awayLine } from '../ipc/plans';
import { useScope } from './scope';

export interface ShownFailure {
  readonly error: UiError;
  /** The title of the change whose run ended in it; null for a refusal before anything ran. */
  readonly change: string | null;
}

/**
 * Shows `error` (a change's end when `change` names it), or clears what shows with `null`. It is
 * an `OnFailed` (ipc/plans.ts) as it is: hand it to whatever reports a screen's failures.
 */
export type ShowFailure = (error: UiError | null, change?: string) => void;

const NOTHING = () => {};

/** The failure the screen shows, and how to show one: see the file's head. */
export function useShownFailure(): readonly [ShownFailure | null, ShowFailure] {
  const scope = useScope();
  const [failure, setFailure] = useState<ShownFailure | null>(null);
  // The registration with the screen's scope of the failure that waits for a lock, if any.
  const waiting = useRef(NOTHING);

  /** `shown` waits for a lock while its screen lives; what waited before waits no more. */
  const wait = useCallback(
    (shown: ShownFailure | null) => {
      waiting.current();
      waiting.current = NOTHING;
      if (shown === null || shown.change === null) return;
      const { error, change } = shown;
      // The session it is shown in: its end is the lock, which ends the screen's scope with it.
      // The screen's own end alone (its dialog closed, its tab) keeps nothing, and so does a
      // screen gone already: onGone runs at once, in the session after that lock.
      const session = sessionScope();
      waiting.current = scope.onGone(() => {
        if (session.gone()) keepEndedAway({ opId: null, ...awayLine(change, error) });
      });
    },
    [scope],
  );

  const show = useCallback<ShowFailure>(
    (error: UiError | null, change?: string) => {
      const next = error === null ? null : { error, change: change ?? null };
      // At once: a lock before the next commit keeps it too.
      wait(next);
      setFailure(next);
    },
    [wait],
  );

  // Shown (again, after its tab hid it): it waits. Hidden, cleared or unmounted: it waits no more.
  useEffect(() => {
    wait(failure);
    return () => {
      waiting.current();
      waiting.current = NOTHING;
    };
  }, [wait, failure]);

  return [failure, show] as const;
}
