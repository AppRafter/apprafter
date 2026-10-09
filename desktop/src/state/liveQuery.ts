// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A query that refreshes while its tab is shown and is not subscribed at all while the tab is
// hidden (spec §4.1: background tabs never poll). Unsubscribed, TanStack Query neither fetches
// nor runs its interval, whether or not React still renders the hidden subtree. D.5 feeds the
// interval from the refresh setting and pauses it while the window is hidden.
import { type QueryKey, type UseQueryResult, useQuery } from '@tanstack/react-query';
import { useTabActive } from './tab';

export interface LiveQueryOptions<T> {
  readonly queryKey: QueryKey;
  readonly queryFn: () => Promise<T>;
  readonly intervalMs: number;
}

export function useLiveQuery<T>({
  queryKey,
  queryFn,
  intervalMs,
}: LiveQueryOptions<T>): UseQueryResult<T> {
  const active = useTabActive();
  return useQuery({ queryKey, queryFn, refetchInterval: intervalMs, subscribed: active });
}
