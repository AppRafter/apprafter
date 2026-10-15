// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  defaultScheduler,
  notifyManager,
  type QueryClient,
  QueryClientProvider,
} from '@tanstack/react-query';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import type { AppInfo } from '../ipc/generated/AppInfo';
import { LOCK_CHANGED, STORE_CHANGED } from '../ipc/generated/events';
import type { LockState } from '../ipc/generated/LockState';
import { attach, operationsSnapshot, resetOperations } from '../ipc/operations';
import { ACTIVITY_INTERVAL_MS } from '../state/lock';
import { STORE_FIELD_PLACEHOLDER } from '../state/lockText';
import { APP_INFO_KEY, PlatformContext, usePlatform } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo, authInfo, lockState } from '../test/fixtures';
import { settleIpc } from '../test/settle';
import { LockGate } from './LockGate';
import { PlatformGate } from './PlatformGate';

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
  notifyManager.setScheduler(defaultScheduler);
  await settleIpc();
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

  test('every lock puts what app_info said in question; the state the app starts in does not', async () => {
    const client = createQueryClient();
    client.setQueryData(APP_INFO_KEY, appInfo());
    const inQuestion = () => client.getQueryState(APP_INFO_KEY)?.isInvalidated;
    const user = gate({ client });
    await screen.findByRole('heading', { name: 'AppRafter is locked' });
    // Locked at start: app_info was read just before, by the gate above this one.
    expect(inQuestion()).toBe(false);
    await user.click(screen.getByRole('button', { name: 'Unlock' }));
    await screen.findByText('the shell');
    expect(inQuestion()).toBe(false);
    // Rust forgets a missing polkit agent on every lock: the field it showed may be gone.
    await changed(lockState({ reason: 'idle', seq: 2 }));
    expect(inQuestion()).toBe(true);
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

  test('scrolling is activity too, heard by a passive listener that never holds a scroll up', async () => {
    status = lockState({ locked: false });
    const added: { type: string; options: unknown }[] = [];
    const original = window.addEventListener;
    window.addEventListener = function (
      this: Window,
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: boolean | AddEventListenerOptions,
    ) {
      added.push({ type, options });
      original.call(this, type, listener, options);
    } as typeof window.addEventListener;
    try {
      gate();
      await screen.findByText('the shell');
    } finally {
      window.addEventListener = original;
    }
    expect(added.find((a) => a.type === 'wheel')?.options).toEqual({
      capture: true,
      passive: true,
    });
    await act(async () => {
      window.dispatchEvent(new WheelEvent('wheel', { deltaY: 40 }));
      await settle();
    });
    expect(calls.filter((c) => c === 'activity')).toHaveLength(1);
  });

  test('after an unlock, what the shell follows as it mounts is subscribed to once', async () => {
    function Follows() {
      useEffect(() => attach(7), []);
      return <div>the shell</div>;
    }
    render(
      <QueryClientProvider client={createQueryClient()}>
        <PlatformContext value={appInfo()}>
          <LockGate>
            <Follows />
          </LockGate>
        </PlatformContext>
      </QueryClientProvider>,
    );
    await screen.findByRole('heading', { name: 'AppRafter is locked' });
    await changed(lockState({ locked: false, sinceMs: 1, seq: 1 }));
    await screen.findByText('the shell');
    await act(settle);
    expect(calls.filter((c) => c === 'op_subscribe')).toHaveLength(1);
    expect(calls).not.toContain('op_unsubscribe');
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

describe('LockGate under the PlatformGate: the field a lock puts in question', () => {
  /** Linux without a polkit agent: the PAM route, and the lock screen's own field. */
  const PAM = appInfo({ auth: authInfo({ method: 'pam', passwordField: true }) });

  test('the commit that shows the lock has no field; the re-read that lands brings it back', async () => {
    let reads = 0;
    let reread!: (info: typeof PAM) => void;
    mockIPC(
      (cmd) => {
        calls.push(cmd);
        if (cmd === 'app_info') {
          reads += 1;
          if (reads === 1) return PAM;
          return new Promise((resolve) => (reread = resolve));
        }
        if (cmd === 'lock_status') return lockState({ locked: false });
        if (cmd === 'plugin:window|is_maximized') return false;
        return null;
      },
      { shouldMockEvents: true },
    );
    render(
      <QueryClientProvider client={createQueryClient()}>
        <PlatformGate>
          <LockGate>
            <ShellProbe />
          </LockGate>
        </PlatformGate>
      </QueryClientProvider>,
    );
    await screen.findByText('the shell');
    // TanStack hands every observer its news on a later task (setTimeout 0): hold them all, so
    // what is on screen is exactly what the commit that shows the lock rendered.
    const held: (() => void)[] = [];
    notifyManager.setScheduler((notify) => {
      held.push(notify);
    });
    const runHeld = () =>
      act(async () => {
        for (const notify of held.splice(0)) notify();
      });
    await act(async () => {
      await emit(LOCK_CHANGED, lockState({ reason: 'idle', seq: 2 }));
    });
    // The lock's own news: the commit that mounts the lock screen.
    await runHeld();
    expect(screen.getByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
    expect(reads).toBe(2);
    expect(screen.queryByLabelText('System password')).toBeNull();
    expect(screen.getByRole('button', { name: 'Unlock' }).textContent).toBe('Unlock');
    // The re-read's news that it started: still nothing that may be stale.
    await runHeld();
    expect(screen.queryByLabelText('System password')).toBeNull();
    // It lands, and says the field is still the way.
    await act(async () => {
      reread(PAM);
      await settle();
    });
    await runHeld();
    expect(screen.getByLabelText('System password')).toBeDefined();
  });
  test('a lock the app starts in shows the field at once: app_info has just answered', async () => {
    mockIPC(
      (cmd) => {
        calls.push(cmd);
        if (cmd === 'app_info') return PAM;
        if (cmd === 'lock_status') return lockState({ reason: 'startup' });
        if (cmd === 'plugin:window|is_maximized') return false;
        return null;
      },
      { shouldMockEvents: true },
    );
    render(
      <QueryClientProvider client={createQueryClient()}>
        <PlatformGate>
          <LockGate>
            <ShellProbe />
          </LockGate>
        </PlatformGate>
      </QueryClientProvider>,
    );
    expect(await screen.findByLabelText('System password')).toBeDefined();
    expect(calls.filter((c) => c === 'app_info')).toHaveLength(1);
  });
});

// D.4a F17: the Shell, which reads app_info again on the window's focus and on Rust's word that
// the store changed, is unmounted while locked; the lock screen lived on what app_info said
// before the lock (P-6, P-7) and kept asking for a lock password a terminal's `lock off` had
// removed (D-5). The first views after the unlock kept it too.
describe('LockGate: the lock screen follows the target store', () => {
  const PAM = appInfo({ auth: authInfo({ method: 'pam', passwordField: true }) });
  const PROTECTED = appInfo({
    secrets: {
      mode: 'locked',
      slots: ['password', 'recovery_key'],
      protected: true,
      store: '~/.config/apprafter',
      problem: null,
    },
  });

  /**
   * app_info answers from `answers` in turn, then the last one again (an answer can be held);
   * `changeTo(info)` makes `info` every later answer: the store changed.
   */
  function platform(answers: (AppInfo | Promise<AppInfo>)[], lock: LockState = lockState()) {
    const queue = [...answers];
    let reads = 0;
    mockIPC(
      (cmd) => {
        calls.push(cmd);
        if (cmd === 'app_info') {
          reads += 1;
          return queue.length > 1 ? queue.shift() : queue[0];
        }
        if (cmd === 'lock_status') return lock;
        if (cmd === 'op_subscribe') return { subscription: calls.length, replay: [] };
        if (cmd === 'plugin:window|is_maximized') return false;
        return null;
      },
      { shouldMockEvents: true },
    );
    return {
      reads: () => reads,
      changeTo: (info: AppInfo) => {
        queue.splice(0, queue.length, info);
      },
    };
  }

  function Probe() {
    const info = usePlatform();
    return <div>{`the shell; secrets ${info.secrets.protected ? 'protected' : 'in files'}`}</div>;
  }

  function gated() {
    render(
      <QueryClientProvider client={createQueryClient()}>
        <PlatformGate>
          <LockGate>
            <Probe />
          </LockGate>
        </PlatformGate>
      </QueryClientProvider>,
    );
  }

  test('locked, the window gaining the focus reads app_info again; the field being typed into stays', async () => {
    let answer!: (info: AppInfo) => void;
    const app = platform([PAM, new Promise<AppInfo>((resolve) => (answer = resolve))]);
    gated();
    expect(await screen.findByLabelText('System password')).toBeDefined();
    expect(app.reads()).toBe(1);
    await act(async () => {
      await emit('tauri://focus', null);
      await settle();
    });
    expect(app.reads()).toBe(2);
    // Nothing put the field in question: it stays while app_info is read.
    expect(screen.getByLabelText('System password')).toBeDefined();
    await act(async () => {
      answer(PAM);
      await settle();
    });
    expect(screen.getByLabelText('System password')).toBeDefined();
  });

  test("locked, Rust's word that the store changed reads app_info again, the field withheld until it answers", async () => {
    let answer!: (info: AppInfo) => void;
    const app = platform([PAM, new Promise<AppInfo>((resolve) => (answer = resolve))]);
    gated();
    expect(await screen.findByLabelText('System password')).toBeDefined();
    await act(async () => {
      await emit(STORE_CHANGED, null);
      await settle();
    });
    expect(app.reads()).toBe(2);
    expect(screen.queryByLabelText('System password')).toBeNull();
    await act(async () => {
      answer(PAM);
      await settle();
    });
    expect(screen.getByLabelText('System password')).toBeDefined();
  });

  test("D-5: a terminal's lock off under the locked app; the lock screen offers the system's Unlock with no focus", async () => {
    platform([PROTECTED, appInfo()]);
    gated();
    expect(await screen.findByPlaceholderText(STORE_FIELD_PLACEHOLDER)).toBeDefined();
    await act(async () => {
      await emit(STORE_CHANGED, null);
      await settle();
    });
    // The store's field goes (its own submit button is called Unlock too); the OS's Unlock stays.
    await waitFor(() =>
      expect(screen.queryByPlaceholderText(STORE_FIELD_PLACEHOLDER) === null).toBe(true),
    );
    expect(screen.getByRole('button', { name: 'Unlock' })).toBeDefined();
  });

  test('an unlock reads app_info again before it follows the operations again; the page shows the answer with no focus', async () => {
    const app = platform([PROTECTED], lockState({ locked: false }));
    expect(app.reads()).toBe(0);
    gated();
    expect(await screen.findByText('the shell; secrets protected')).toBeDefined();
    attach(7);
    await changed(lockState({ reason: 'manual', seq: 1 }));
    // The store changes while locked, unheard (a folder below the root: P-6).
    app.changeTo(appInfo());
    const before = calls.length;
    await changed(lockState({ locked: false, seq: 2 }));
    const after = calls.slice(before);
    expect(after.indexOf('app_info')).toBeGreaterThanOrEqual(0);
    expect(after.indexOf('app_info')).toBeLessThan(after.indexOf('op_subscribe'));
    expect(await screen.findByText('the shell; secrets in files')).toBeDefined();
  });
});
