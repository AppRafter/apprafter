// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The one QueryClient (brief §4.4): no retries (a cluster read already times out on its own,
// and an IPC refusal is an answer), and no refetch on window focus (live data refreshes on its
// own interval; D.5 wires focus to the window's visibility instead).
import { QueryClient } from '@tanstack/react-query';

export function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
  });
}
