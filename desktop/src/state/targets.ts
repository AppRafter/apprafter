// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The target store as the screens read it: one list, one report per target. Both keys sit
// outside KEPT_WHILE_LOCKED, so a lock drops them with the rest.
import type { QueryClient } from '@tanstack/react-query';
import * as api from '../ipc/api';
import { useLiveQuery } from './liveQuery';

export const TARGETS_KEY = ['targets'] as const;
export const targetKey = (name: string) => ['target', name] as const;
/**
 * How often a shown Targets view or Target screen reads the store again: the Settings default
 * interval (D.5 feeds the setting). A local read of a few small files.
 */
export const STORE_REFRESH_MS = 5_000;

export const useTargetList = () =>
  useLiveQuery({ queryKey: TARGETS_KEY, queryFn: api.targetList, intervalMs: STORE_REFRESH_MS });

export const useTargetReport = (name: string) =>
  useLiveQuery({
    queryKey: targetKey(name),
    queryFn: () => api.targetShow(name),
    intervalMs: STORE_REFRESH_MS,
  });

/** After a mutation: the list again; the reports of `gone` (a renamed or removed name) dropped. */
export function refreshTargets(client: QueryClient, ...gone: string[]): void {
  void client.invalidateQueries({ queryKey: TARGETS_KEY });
  for (const name of gone) client.removeQueries({ queryKey: targetKey(name) });
}
