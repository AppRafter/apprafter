// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The lock as the page knows it. Rust is authoritative: the ['lock'] query holds its LockState,
// fed by lock_status, by every `lock-changed` event, and by the answers of lock_now and unlock.
// An answer and its event carry the same state and land on the same entry, so the order they
// reach the page in changes nothing.
//
// At startup nothing may slip between the read and the listener: the `lock-changed` listener is
// registered first, and lock_status is asked only then. An event can still land while that
// answer is on its way; of the two the newer state wins, by `sinceMs` (when that state began),
// and on a tie — the same state, seen twice — the answer.
import {
  type QueryClient,
  type UseQueryResult,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import { useEffect, useMemo, useState } from 'react';
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
  // Resolved once the listener is in place (or could not be): the read waits for it.
  const [listening] = useState(() => {
    let ready!: () => void;
    const promise = new Promise<void>((resolve) => (ready = resolve));
    return { promise, ready };
  });
  const query = useQuery({
    queryKey: LOCK_KEY,
    queryFn: async () => {
      await listening.promise;
      const answer = await lockStatus();
      const heard = client.getQueryData<LockState>(LOCK_KEY);
      return heard !== undefined && heard.sinceMs > answer.sinceMs ? heard : answer;
    },
    staleTime: Infinity,
  });
  useEffect(() => {
    let mounted = true;
    let off: (() => void) | undefined;
    onLockChanged((state) => client.setQueryData(LOCK_KEY, state))
      .then((unlisten) => {
        if (mounted) off = unlisten;
        else unlisten();
      })
      .catch((error: unknown) => console.error('lock-changed is not heard:', error))
      .finally(listening.ready);
    return () => {
      mounted = false;
      off?.();
    };
  }, [client, listening]);
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

/** Why the app lock is not in effect: nothing to verify the owner with, or the owner's choice. */
export type LockOff = 'no_auth' | 'turned_off';

/** What the shell says, under its title bar, while the OS offers no way to verify the owner. */
export const NO_AUTH_NOTICE =
  'This computer offers no system authentication AppRafter can use, so the app lock is off.';

const LOCK_OFF_MESSAGES: Record<LockOff, string> = {
  no_auth: NO_AUTH_NOTICE,
  turned_off: 'The app lock is off: turn on Require unlock in Settings to use it.',
};

/**
 * Why the lock is not in effect — Rust locks only with both the setting on and a way to verify
 * the owner — or null when it is, or the settings are not read yet (Rust decides then).
 */
export function lockOff(authAvailable: boolean, lockEnabled: boolean | undefined): LockOff | null {
  if (!authAvailable) return 'no_auth';
  if (lockEnabled === false) return 'turned_off';
  return null;
}

/** What Mod+L says instead of locking. */
export function lockOffMessage(off: LockOff): string {
  return LOCK_OFF_MESSAGES[off];
}

/** A lock leaves no cluster data in the page: every query but the lock screen's own goes. */
export function dropUnlockedData(client: QueryClient): void {
  client.removeQueries({ predicate: (query) => !KEPT_WHILE_LOCKED.has(query.queryKey[0]) });
}
