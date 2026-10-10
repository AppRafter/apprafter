// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One read a component runs at a time (a token verify, a catalogue, latencies, doctor, whoami):
// its state, a cancel, and the guarantee that what runs is cancelled when its screen goes — its
// dialog closed, its tab closed, or the lock (ipc/lifecycle.ts; Rust also cancels reads on a
// lock, so the cancel here is then refused as locked, and that refusal is expected). Never on an
// effect cleanup: a view <Activity> hides runs those without going, and a read a hidden screen
// started runs on, its end settling the state for when the screen is shown again. A run that is
// no longer the component's (superseded by another run, reset, or its screen gone) never changes
// its state; a result it still brings is handed to `onUnused`, so what it holds is released (a
// verify's draft).
import { useCallback, useMemo, useRef, useState } from 'react';
import type { OpId } from '../ipc/generated/OpId';
import type { UiError } from '../ipc/generated/UiError';
import { cancel as cancelOp } from '../ipc/operations';
import { failureOf, isCancelled, reportUnlessLocked, runRead } from '../ipc/plans';
import { useScope } from './scope';

export type ReadState<T> =
  | { readonly status: 'idle' }
  | { readonly status: 'running'; readonly opId: OpId | null }
  | { readonly status: 'done'; readonly data: T }
  | { readonly status: 'failed'; readonly error: UiError }
  | { readonly status: 'cancelled' };

export interface ReadHandle<T> {
  readonly state: ReadState<T>;
  /**
   * Starts the read (cancelling one that runs); its data, or null when it did not complete or is
   * no longer this component's. `onUnused` gets the data of a run that completed when it was no
   * longer this component's. Once its screen is gone a run starts nothing.
   */
  readonly run: (start: () => Promise<OpId>, onUnused?: (data: T) => void) => Promise<T | null>;
  readonly cancel: () => void;
  readonly reset: () => void;
}

interface Run {
  opId: OpId | null;
  cancelled: boolean;
}

export function useRead<T>(): ReadHandle<T> {
  const scope = useScope();
  const [state, setState] = useState<ReadState<T>>({ status: 'idle' });
  const current = useRef<Run | null>(null);

  const stop = useCallback((run: Run | null) => {
    if (run === null || run.cancelled) return;
    run.cancelled = true;
    if (run.opId !== null) cancelOp(run.opId).catch(reportUnlessLocked(`op_cancel ${run.opId}`));
  }, []);

  /** `run` is not the component's any more: cancelled, and its end goes nowhere. */
  const drop = useCallback(
    (run: Run) => {
      if (current.current === run) current.current = null;
      stop(run);
    },
    [stop],
  );

  const run = useCallback(
    async (start: () => Promise<OpId>, onUnused?: (data: T) => void): Promise<T | null> => {
      if (scope.gone()) return null;
      const was = current.current;
      if (was !== null) drop(was);
      const mine: Run = { opId: null, cancelled: false };
      current.current = mine;
      // Its screen goes (dialog or tab closed, the lock): the run is cancelled and not used.
      const offGone = scope.onGone(() => drop(mine));
      const live = () => current.current === mine;
      setState({ status: 'running', opId: null });
      try {
        const data = await runRead<T>(start, (opId) => {
          mine.opId = opId;
          if (mine.cancelled) cancelOp(opId).catch(reportUnlessLocked(`op_cancel ${opId}`));
          else if (live()) setState({ status: 'running', opId });
        });
        if (!live()) {
          onUnused?.(data);
          return null;
        }
        current.current = null;
        setState({ status: 'done', data });
        return data;
      } catch (reason) {
        if (!live()) return null;
        current.current = null;
        setState(
          isCancelled(reason)
            ? { status: 'cancelled' }
            : { status: 'failed', error: failureOf(reason) },
        );
        return null;
      } finally {
        offGone();
      }
    },
    [scope, drop],
  );

  const cancel = useCallback(() => stop(current.current), [stop]);
  const reset = useCallback(() => {
    const was = current.current;
    if (was !== null) drop(was);
    setState({ status: 'idle' });
  }, [drop]);

  return useMemo(() => ({ state, run, cancel, reset }), [state, run, cancel, reset]);
}
