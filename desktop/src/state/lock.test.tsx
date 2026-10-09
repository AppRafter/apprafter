// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The lock state cannot miss a change at startup: the `lock-changed` listener is in place
// before `lock_status` is asked, and an event that lands while the answer is on its way is not
// overwritten by an older answer.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, renderHook } from '@testing-library/react';
import type { ReactNode } from 'react';
import { LOCK_CHANGED } from '../ipc/generated/events';
import type { LockState } from '../ipc/generated/LockState';
import { lockState } from '../test/fixtures';
import { useLockState } from './lock';
import { createQueryClient } from './queryClient';

interface TauriInternals {
  runCallback(id: number, data: unknown): void;
}

let order: string[];
let handler: number | undefined;
let status: { promise: Promise<LockState>; resolve: (state: LockState) => void };

beforeEach(() => {
  order = [];
  handler = undefined;
  let resolve!: (state: LockState) => void;
  status = { promise: new Promise((res) => (resolve = res)), resolve };
  // Events mocked by hand: the test sees when the listener registers and delivers to it.
  mockIPC((cmd, args) => {
    order.push(cmd);
    if (cmd === 'plugin:event|listen') {
      handler = (args as { handler: number }).handler;
      return handler;
    }
    if (cmd === 'lock_status') return status.promise;
    return null;
  });
});

afterEach(() => {
  cleanup();
  clearMocks();
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function deliver(state: LockState) {
  if (handler === undefined) throw new Error('no lock-changed listener');
  const internals = (window as unknown as { __TAURI_INTERNALS__: TauriInternals })
    .__TAURI_INTERNALS__;
  internals.runCallback(handler, { event: LOCK_CHANGED, id: 1, payload: state });
}

function mount() {
  const client = createQueryClient();
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return renderHook(() => useLockState(), { wrapper });
}

test('the listener is registered before lock_status is asked', async () => {
  mount();
  await act(settle);
  expect(order.slice(0, 2)).toEqual(['plugin:event|listen', 'lock_status']);
});

test('an event during the startup read wins over an older answer', async () => {
  const { result } = mount();
  await act(settle);
  act(() => deliver(lockState({ locked: false, sinceMs: 2_000 })));
  await act(async () => {
    status.resolve(lockState({ locked: true, reason: 'startup', sinceMs: 1_000 }));
    await settle();
  });
  expect(result.current.data).toEqual(lockState({ locked: false, sinceMs: 2_000 }));
});

test('an answer newer than an earlier event wins', async () => {
  const { result } = mount();
  await act(settle);
  act(() => deliver(lockState({ locked: false, sinceMs: 1_000 })));
  await act(async () => {
    status.resolve(lockState({ locked: true, reason: 'idle', sinceMs: 3_000 }));
    await settle();
  });
  expect(result.current.data).toEqual(lockState({ locked: true, reason: 'idle', sinceMs: 3_000 }));
});
