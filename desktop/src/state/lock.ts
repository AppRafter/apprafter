// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The lock as the page knows it. Rust is authoritative: the ['lock'] query holds its LockState,
// fed by lock_status, by every `lock-changed` event, and by the answers of lock_now and unlock.
//
// Never back to an older state: wherever the entry is written, a state that began (`sinceMs`)
// before the one held is dropped; on a tie — the same state seen twice, or a change within it
// such as its auto-lock minutes — the one arriving is taken. So an answer and its event land in
// either order, and an unlock's answer that a later lock overtook changes nothing.
//
// Nothing missed: lock_status is asked only once the `lock-changed` listener is in place, and
// every registration is followed by a read — the first one releases the query's own read, any
// later one (a remount) reads again — since what changed while nothing listened is heard of no
// other way. Under StrictMode's double mount, the registration its cleanup drops releases
// nothing: the read waits for the one that stays.
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

/** `incoming`, unless it began before `held`. */
function newer(held: LockState | undefined, incoming: LockState): LockState {
  return held !== undefined && incoming.sinceMs < held.sinceMs ? held : incoming;
}

/** Writes `state` to the entry, unless the entry holds a newer one. */
function write(client: QueryClient, state: LockState): void {
  client.setQueryData<LockState>(LOCK_KEY, (held) => newer(held, state));
}

/** Resolved by the first registration of a listener that stays; per query client. */
interface FirstListener {
  readonly promise: Promise<void>;
  readonly resolve: () => void;
  registered: boolean;
}

const firstListeners = new WeakMap<QueryClient, FirstListener>();

function firstListenerOf(client: QueryClient): FirstListener {
  let first = firstListeners.get(client);
  if (first === undefined) {
    let resolve!: () => void;
    const promise = new Promise<void>((res) => (resolve = res));
    first = { promise, resolve, registered: false };
    firstListeners.set(client, first);
  }
  return first;
}

export function useLockState(): UseQueryResult<LockState> {
  const client = useQueryClient();
  const query = useQuery({
    queryKey: LOCK_KEY,
    queryFn: async () => {
      await firstListenerOf(client).promise;
      const answer = await lockStatus();
      return newer(client.getQueryData<LockState>(LOCK_KEY), answer);
    },
    staleTime: Infinity,
  });
  useEffect(() => {
    let mounted = true;
    let off: (() => void) | undefined;
    // In place (or it could not be, and the read must not wait for ever): read, now or again.
    const listening = () => {
      const first = firstListenerOf(client);
      if (!first.registered) {
        first.registered = true;
        first.resolve();
      } else {
        void client.refetchQueries({ queryKey: LOCK_KEY });
      }
    };
    onLockChanged((state) => write(client, state))
      .then((unlisten) => {
        if (!mounted) {
          unlisten();
          return;
        }
        off = unlisten;
        listening();
      })
      .catch((error: unknown) => {
        console.error('lock-changed is not heard:', error);
        if (mounted) listening();
      });
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
        write(client, await lockNow());
      },
      unlock: async () => {
        write(client, await unlock());
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
