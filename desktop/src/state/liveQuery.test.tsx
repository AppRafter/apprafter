// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// No background polling (spec §4.1): a tab that is not shown issues no IPC at all, whatever
// React does with its hidden subtree — the query is not subscribed while its tab is hidden.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, render } from '@testing-library/react';
import { useLiveQuery } from './liveQuery';
import { createQueryClient } from './queryClient';
import { TabContext } from './tab';

let calls: string[];

beforeEach(() => {
  calls = [];
  mockIPC((_, args) => {
    calls.push((args as { from: string }).from);
    return [];
  });
});

afterEach(() => {
  cleanup();
  clearMocks();
});

function Poller({ from }: { from: string }) {
  useLiveQuery({
    queryKey: ['poll', from],
    queryFn: () => invoke('op_list', { from }),
    intervalMs: 10,
  });
  return null;
}

const tab = (key: string) => ({ key, target: key, section: 'overview' as const });

test('only the shown tab polls; the hidden one issues no IPC across many intervals', async () => {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <TabContext value={{ tab: tab('shown'), active: true }}>
        <Poller from="shown" />
      </TabContext>
      <TabContext value={{ tab: tab('hidden'), active: false }}>
        <Poller from="hidden" />
      </TabContext>
    </QueryClientProvider>,
  );
  await act(() => new Promise((resolve) => setTimeout(resolve, 120)));
  expect(calls.filter((c) => c === 'hidden')).toHaveLength(0);
  expect(calls.filter((c) => c === 'shown').length).toBeGreaterThan(3);
});
