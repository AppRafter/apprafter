// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The target store as the screens read it: one list, one report per target. Both keys sit
// outside KEPT_WHILE_LOCKED, so a lock drops them with the rest.
import type { QueryClient } from '@tanstack/react-query';
import * as api from '../ipc/api';
import type { TargetListReport } from '../ipc/generated/TargetListReport';
import { useLiveQuery } from './liveQuery';

export const TARGETS_KEY = ['targets'] as const;
export const targetKey = (name: string) => ['target', name] as const;
/**
 * How often a shown Targets view or Target screen reads the store again: the Settings default
 * interval (D.5 feeds the setting). A local read of a few small files.
 */
export const STORE_REFRESH_MS = 5_000;

/**
 * Every target the store holds, by name, as the core lists them (`list_target_names`, sorted):
 * the readable and the unreadable alike. The Security page's covered line (WI-476): the lock is
 * the store's, so a target whose config.yaml cannot be read is covered as much as any.
 */
export function storeTargetNames(report: TargetListReport): string[] {
  const names = [...report.targets, ...report.unreadable].map((target) => target.name);
  return names.sort();
}

export const useTargetList = () =>
  useLiveQuery({ queryKey: TARGETS_KEY, queryFn: api.targetList, intervalMs: STORE_REFRESH_MS });

export const useTargetReport = (name: string) =>
  useLiveQuery({
    queryKey: targetKey(name),
    queryFn: () => api.targetShow(name),
    intervalMs: STORE_REFRESH_MS,
  });

/**
 * After a mutation: the reports of `gone` (a renamed or removed name) dropped, then the list and
 * every report shown read again (a renewal changes the report, a rename or remove the list).
 */
export function refreshTargets(client: QueryClient, ...gone: string[]): void {
  for (const name of gone) client.removeQueries({ queryKey: targetKey(name) });
  void client.invalidateQueries({ queryKey: TARGETS_KEY });
  void client.invalidateQueries({ queryKey: ['target'] });
}
