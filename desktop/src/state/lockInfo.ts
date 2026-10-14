// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What Settings › Lock shows of the store and this computer (lock_info, D.4a §3.14): read when the
// section opens and again when a flow ends. A lock removes it with every query but the lock
// screen's (state/lock.ts KEPT_WHILE_LOCKED).
import { type QueryClient, useQuery } from '@tanstack/react-query';
import { lockInfo } from '../ipc/api';
import { rereadAppInfo } from './platform';

export const LOCK_INFO_KEY = ['lock-info'] as const;

export function useLockInfo() {
  return useQuery({ queryKey: LOCK_INFO_KEY, queryFn: lockInfo, staleTime: 0 });
}

/** A flow ended, or was cancelled: where the secrets are, and what the section shows, may have changed. */
export function rereadLock(client: QueryClient): void {
  void client.invalidateQueries({ queryKey: LOCK_INFO_KEY });
  rereadAppInfo(client, false);
}
