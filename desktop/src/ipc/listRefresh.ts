// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// op_list is a snapshot: an operation nobody follows changes state with no event reaching the
// page, and "N running" and the tabs' spinners would show it running for ever. So while any
// listed operation runs, the list is read again every LIST_REFRESH_MS; idle, nothing is polled.
// A followed operation that ends reads it at once (the list may hold what it started, or
// what ended with it).
import { useEffect, useMemo, useRef } from 'react';
import { IpcError } from './api';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { OpId } from './generated/OpId';
import { refreshList, useOperations } from './operations';

export const LIST_REFRESH_MS = 5_000;

function refresh() {
  refreshList().catch((error: unknown) => {
    // A lock that landed meanwhile: the shell goes, and its refreshes with it.
    if (!(error instanceof IpcError && error.error.code === DESKTOP_ERROR_CODES.LOCKED)) {
      console.error('op_list failed:', error);
    }
  });
}

/** Keeps op_list's summaries current while the shell shows them; `every` is for tests. */
export function useListRefresh(every: number = LIST_REFRESH_MS): void {
  const operations = useOperations();
  const running = useMemo(
    () => [...operations.values()].some((op) => op.summary?.state === 'running'),
    [operations],
  );
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(refresh, every);
    return () => clearInterval(timer);
  }, [running, every]);

  const ended = useRef(new Set<OpId>());
  useEffect(() => {
    let fresh = false;
    for (const op of operations.values()) {
      if (op.end !== null && !ended.current.has(op.opId)) {
        ended.current.add(op.opId);
        fresh = true;
      }
    }
    if (fresh) refresh();
  }, [operations]);
}
