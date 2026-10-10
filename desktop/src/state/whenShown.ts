// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// An end a screen applies with effects — a toast, its own close, the caller's onDone — applied
// when the screen is shown: at once while it is, else on its next show. A view <Activity> hides
// keeps the screen's state but runs none of its effects, so an end that arrives while its tab is
// hidden waits here, never in the app's notices (D.3e review 17-20). If the screen goes first
// (its tab closed, the lock: ipc/lifecycle.ts), `orElse` gets the end instead — the app shows it
// (ipc/away.ts), so it is never dropped unseen.
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useScope } from './scope';

interface Pending<T> {
  readonly value: T;
}

/** Returns `when(value)`: `apply(value)` once the screen is shown, or `orElse(value)` if it goes. */
export function useWhenShown<T>(
  apply: (value: T) => void,
  orElse: (value: T) => void,
): (value: T) => void {
  const scope = useScope();
  const [pending, setPending] = useState<Pending<T> | null>(null);
  const latest = useRef({ apply, orElse });
  useLayoutEffect(() => {
    latest.current = { apply, orElse };
  });
  // Each end still waiting, with its registration on the scope.
  const waiting = useRef(new Map<Pending<T>, () => void>());

  useEffect(() => {
    if (pending === null) return;
    const off = waiting.current.get(pending);
    // Applied already (StrictMode runs a revealed effect twice), or given to orElse.
    if (off === undefined) return;
    off();
    waiting.current.delete(pending);
    setPending(null);
    latest.current.apply(pending.value);
  }, [pending]);

  return useCallback(
    (value: T) => {
      const entry: Pending<T> = { value };
      if (scope.gone()) {
        latest.current.orElse(value);
        return;
      }
      waiting.current.set(
        entry,
        scope.onGone(() => {
          if (!waiting.current.delete(entry)) return;
          latest.current.orElse(value);
        }),
      );
      setPending(entry);
    },
    [scope],
  );
}
