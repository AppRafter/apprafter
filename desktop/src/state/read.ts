// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One read a component runs at a time (a token verify, a catalogue, latencies, doctor, whoami):
// its state, a cancel, and the guarantee that what runs is cancelled when the component goes — a
// closed dialog, or the shell unmounted by a lock (Rust also cancels reads on a lock; then the
// cancel here is refused as locked, and that refusal is expected).
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { OpId } from '../ipc/generated/OpId';
import type { UiError } from '../ipc/generated/UiError';
import { cancel as cancelOp } from '../ipc/operations';
import { failureOf, isCancelled, reportUnlessLocked, runRead } from '../ipc/plans';

export type ReadState<T> =
  | { readonly status: 'idle' }
  | { readonly status: 'running'; readonly opId: OpId | null }
  | { readonly status: 'done'; readonly data: T }
  | { readonly status: 'failed'; readonly error: UiError }
  | { readonly status: 'cancelled' };

export interface ReadHandle<T> {
  readonly state: ReadState<T>;
  /** Starts the read (cancelling one that runs); its data, or null when it did not complete. */
  readonly run: (start: () => Promise<OpId>) => Promise<T | null>;
  readonly cancel: () => void;
  readonly reset: () => void;
}

interface Run {
  opId: OpId | null;
  cancelled: boolean;
}

export function useRead<T>(): ReadHandle<T> {
  const [state, setState] = useState<ReadState<T>>({ status: 'idle' });
  const current = useRef<Run | null>(null);

  const stop = useCallback((run: Run | null) => {
    if (run === null || run.cancelled) return;
    run.cancelled = true;
    if (run.opId !== null) cancelOp(run.opId).catch(reportUnlessLocked(`op_cancel ${run.opId}`));
  }, []);

  // The component goes (closed, or the shell unmounted by a lock): what runs is cancelled.
  useEffect(() => () => stop(current.current), [stop]);

  const run = useCallback(
    async (start: () => Promise<OpId>): Promise<T | null> => {
      stop(current.current);
      const mine: Run = { opId: null, cancelled: false };
      current.current = mine;
      const live = () => current.current === mine;
      setState({ status: 'running', opId: null });
      try {
        const data = await runRead<T>(start, (opId) => {
          mine.opId = opId;
          if (mine.cancelled) cancelOp(opId).catch(reportUnlessLocked(`op_cancel ${opId}`));
          else if (live()) setState({ status: 'running', opId });
        });
        if (!live()) return null;
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
      }
    },
    [stop],
  );

  const cancel = useCallback(() => stop(current.current), [stop]);
  const reset = useCallback(() => {
    const was = current.current;
    current.current = null;
    stop(was);
    setState({ status: 'idle' });
  }, [stop]);

  return useMemo(() => ({ state, run, cancel, reset }), [state, run, cancel, reset]);
}
