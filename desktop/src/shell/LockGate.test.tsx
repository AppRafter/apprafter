// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { type QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { LOCK_CHANGED } from '../ipc/generated/events';
import type { LockState } from '../ipc/generated/LockState';
import { attach, operationsSnapshot, resetOperations } from '../ipc/operations';
import { ACTIVITY_INTERVAL_MS } from '../state/lock';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo, lockState } from '../test/fixtures';
import { LockGate } from './LockGate';

let calls: string[];
let status: LockState;
let unlockAnswer: () => unknown;
let mounts: number;

beforeEach(() => {
  calls = [];
  status = lockState();
  unlockAnswer = () => lockState({ locked: false });
  mounts = 0;
  mockWindows('main');
  mockIPC(
    (cmd) => {
      calls.push(cmd);
      if (cmd === 'lock_status') return status;
      if (cmd === 'unlock') return unlockAnswer();
      if (cmd === 'op_subscribe') return { subscription: calls.length, replay: [] };
      if (cmd === 'plugin:window|is_maximized') return false;
      return null;
    },
    { shouldMockEvents: true },
  );
});

afterEach(async () => {
  cleanup();
  await settle();
  resetOperations();
  clearMocks();
});

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function ShellProbe() {
  useEffect(() => {
    mounts += 1;
  }, []);
  return <div>the shell</div>;
}

function gate({
  client = createQueryClient(),
  now,
}: {
  client?: QueryClient;
  now?: () => number;
} = {}) {
  render(
    <QueryClientProvider client={client}>
      <PlatformContext value={appInfo()}>
        <LockGate {...(now !== undefined && { now })}>
          <ShellProbe />
        </LockGate>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}

const changed = (state: LockState) =>
  act(async () => {
    await emit(LOCK_CHANGED, state);
    await settle();
  });

describe('LockGate', () => {
  test('locked, only the lock screen renders: the shell is not mounted at all', async () => {
    gate();
    expect(await screen.findByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
    expect(screen.queryByText('the shell')).toBeNull();
    expect(mounts).toBe(0);
  });

  test('unlocked, the shell renders and the lock screen does not', async () => {
    status = lockState({ locked: false });
    gate();
    expect(await screen.findByText('the shell')).toBeDefined();
    expect(screen.queryByRole('heading', { name: 'AppRafter is locked' })).toBeNull();
  });

  test('a lock unmounts the shell and drops cached data, keeping what the lock screen reads', async () => {
    status = lockState({ locked: false });
    const client = createQueryClient();
    client.setQueryData(['target', 'prod-eu'], { nodes: 3 });
    client.setQueryData(['app-info'], appInfo());
    gate({ client });
    await screen.findByText('the shell');
    await changed(lockState({ reason: 'manual' }));
    expect(screen.getByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
    expect(screen.queryByText('the shell')).toBeNull();
    expect(client.getQueryData(['target', 'prod-eu'])).toBeUndefined();
    expect(client.getQueryData(['app-info'])).toBeDefined();
  });

  test('an unlock mounts the shell once when its answer comes before the event', async () => {
    const user = gate();
    await user.click(await screen.findByRole('button', { name: 'Unlock' }));
    await act(settle);
    expect(screen.getByText('the shell')).toBeDefined();
    await changed(lockState({ locked: false }));
    expect(mounts).toBe(1);
  });

  test('…and once when the event comes before the answer', async () => {
    let answer!: (state: LockState) => void;
    unlockAnswer = () => new Promise((resolve) => (answer = resolve));
    const user = gate();
    await user.click(await screen.findByRole('button', { name: 'Unlock' }));
    await changed(lockState({ locked: false }));
    expect(screen.getByText('the shell')).toBeDefined();
    await act(async () => {
      answer(lockState({ locked: false }));
      await settle();
    });
    expect(mounts).toBe(1);
  });

  test(`activity is reported at most once per ${ACTIVITY_INTERVAL_MS / 1000} s, and only while unlocked`, async () => {
    status = lockState({ locked: false });
    let clock = 1_000_000;
    const user = gate({ now: () => clock });
    await screen.findByText('the shell');
    await user.keyboard('a');
    await user.click(screen.getByText('the shell'));
    expect(calls.filter((c) => c === 'activity')).toHaveLength(1);
    clock += ACTIVITY_INTERVAL_MS - 1;
    await user.keyboard('b');
    expect(calls.filter((c) => c === 'activity')).toHaveLength(1);
    clock += 1;
    await user.keyboard('c');
    expect(calls.filter((c) => c === 'activity')).toHaveLength(2);
    await changed(lockState({ reason: 'manual' }));
    clock += ACTIVITY_INTERVAL_MS;
    await user.keyboard('d');
    expect(calls.filter((c) => c === 'activity')).toHaveLength(2);
  });

  test('a lock forgets what the operations store showed; the unlock follows it again', async () => {
    status = lockState({ locked: false });
    gate();
    await screen.findByText('the shell');
    attach(7);
    await act(settle);
    expect(operationsSnapshot().get(7)?.live).toBe(true);
    await changed(lockState({ reason: 'manual' }));
    expect(operationsSnapshot().get(7)?.live).toBe(false);
    await changed(lockState({ locked: false }));
    await act(settle);
    expect(calls.filter((c) => c === 'op_subscribe')).toHaveLength(2);
    expect(operationsSnapshot().get(7)?.live).toBe(true);
  });
});
