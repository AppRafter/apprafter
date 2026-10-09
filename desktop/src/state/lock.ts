// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The lock as the page knows it. Rust is authoritative: the ['lock'] query holds its LockState,
// fed by lock_status, by every `lock-changed` event, and by the answers of lock_now and unlock.
// An answer and its event carry the same state and land on the same entry, so the order they
// reach the page in changes nothing.
import {
  type QueryClient,
  type UseQueryResult,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import { useEffect, useMemo } from 'react';
import { lockNow, lockStatus, unlock } from '../ipc/api';
import { onLockChanged } from '../ipc/events';
import type { LockState } from '../ipc/generated/LockState';

export const LOCK_KEY = ['lock'] as const;

/** At most one `activity` ping per this long (Rust runs the idle timer). */
export const ACTIVITY_INTERVAL_MS = 30_000;

/** What the lock screen itself reads; a lock removes every other query (spec §4.3). */
const KEPT_WHILE_LOCKED: ReadonlySet<unknown> = new Set(['lock', 'settings', 'app-info']);

export function useLockState(): UseQueryResult<LockState> {
  const client = useQueryClient();
  const query = useQuery({ queryKey: LOCK_KEY, queryFn: lockStatus, staleTime: Infinity });
  useEffect(() => {
    let mounted = true;
    let off: (() => void) | undefined;
    onLockChanged((state) => client.setQueryData(LOCK_KEY, state))
      .then((unlisten) => {
        if (mounted) off = unlisten;
        else unlisten();
      })
      .catch((error: unknown) => console.error('lock-changed is not heard:', error));
    return () => {
      mounted = false;
      off?.();
    };
  }, [client]);
  return query;
}

/** Lock and unlock; each answer is the new state, written where the event writes it. */
export function useLockActions(): { lock: () => Promise<void>; unlock: () => Promise<void> } {
  const client = useQueryClient();
  return useMemo(
    () => ({
      lock: async () => {
        client.setQueryData(LOCK_KEY, await lockNow());
      },
      unlock: async () => {
        client.setQueryData(LOCK_KEY, await unlock());
      },
    }),
    [client],
  );
}

/** A lock leaves no cluster data in the page: every query but the lock screen's own goes. */
export function dropUnlockedData(client: QueryClient): void {
  client.removeQueries({ predicate: (query) => !KEPT_WHILE_LOCKED.has(query.queryKey[0]) });
}
