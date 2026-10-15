// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A confirmed plan runs on through a lock (Rust keeps it; app.rs), while the lock unmounts the
// screen that started it. Its end must still be shown: after the unlock, once it arrives.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { PlanConfirm } from '../components/PlanConfirm';
import { type Schedule, ToastProvider, ToastViewport, useToast } from '../components/Toast';
import { endedAwaySnapshot, keepEndedAway, resetEndedAway } from '../ipc/away';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import { LOCK_CHANGED } from '../ipc/generated/events';
import { resetOperations } from '../ipc/operations';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo, lockState, planView } from '../test/fixtures';
import { settleIpc } from '../test/settle';
import { EndedAwayNotices } from './EndedAway';
import { LockGate } from './LockGate';
import { useOverlay, ViewFrame } from './ViewFrame';

const ALLOWED_LOCKED = [
  'lock_status',
  'app_info',
  'settings_get',
  'unlock',
  'quit',
  'window_ready',
];
let calls: { cmd: string; args: Record<string, unknown> }[];
let locked: boolean;
/** Rust's end of op 900, replayed to a follow: none yet, then the failure. */
let ended: boolean;
/** D.4a F18: the end that replay carries, a failure unless a test says it completed. */
let endedDone: boolean;
/** D.4a F18: Rust's answer to op_execute, when a test holds it (else: the op runs). */
let execute: (() => Promise<unknown>) | null;
/** D.4a N5: the run completes at once, its end sent on op_execute's channel while it is shown. */
let endsAtOnce: boolean;
let opened: boolean;

beforeEach(() => {
  calls = [];
  locked = false;
  ended = false;
  endedDone = false;
  execute = null;
  endsAtOnce = false;
  opened = false;
  mockWindows('main');
  mockIPC(
    (cmd, raw) => {
      const args = (raw ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args });
      if (cmd === 'lock_status') return lockState({ locked });
      if (locked && !cmd.startsWith('plugin:') && !ALLOWED_LOCKED.includes(cmd)) {
        return Promise.reject({
          code: DESKTOP_ERROR_CODES.LOCKED,
          message: 'locked',
          help: null,
          causes: [],
          fields: {},
        });
      }
      if (cmd === 'op_execute') {
        if (execute !== null) return execute();
        if (endsAtOnce) {
          // Its end on the execute's channel, as Rust sends it (test/ipc.ts does the same).
          const channel = args.onEvent as { id: number };
          const internals = (
            window as unknown as {
              __TAURI_INTERNALS__: { runCallback(id: number, data: unknown): void };
            }
          ).__TAURI_INTERNALS__;
          setTimeout(() => {
            internals.runCallback(channel.id, {
              index: 0,
              message: { kind: 'finished', outcome: { status: 'completed', result: null } },
            });
          }, 0);
        }
        return 2; // Rust runs it
      }
      if (cmd === 'op_subscribe') {
        return {
          subscription: 3,
          replay: ended
            ? endedDone
              ? [{ kind: 'finished', outcome: { status: 'completed', result: null } }]
              : [
                  {
                    kind: 'failed',
                    error: {
                      code: 'apprafter::provider::sku_unavailable',
                      message: 'cx22 is sold out in nbg1',
                      help: null,
                      causes: [],
                      fields: {},
                    },
                  },
                ]
            : [],
        };
      }
      return null;
    },
    { shouldMockEvents: true },
  );
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  resetEndedAway();
  clearMocks();
});

/** What the confirm's page shows of a completed run, as the app's pages do (D.4a N5). */
const RENAMED = 'Renamed prod-eu to prod.';

/**
 * Opens the confirm in the first shell only: the shell after the unlock is a new one. Its page
 * toasts the run's end, marked as one, as every page that runs a change does.
 */
function OpenConfirm() {
  const show = useOverlay();
  const toast = useToast();
  useEffect(() => {
    if (opened) return;
    opened = true;
    show((close) => (
      <PlanConfirm
        view={planView({ opId: 900, title: 'Rename prod-eu' })}
        title="Rename prod-eu?"
        confirmLabel="Rename"
        auth={null}
        onDone={() => toast({ message: RENAMED, end: 'done' })}
        onFailed={() => {}}
        onClose={close}
      />
    ));
  }, [show, toast]);
  return null;
}

const changed = (isLocked: boolean) =>
  act(async () => {
    locked = isLocked;
    await emit(
      LOCK_CHANGED,
      lockState({ locked: isLocked, reason: isLocked ? 'manual' : null, seq: isLocked ? 1 : 2 }),
    );
    await new Promise((r) => setTimeout(r, 0));
  });

const discarded = () => calls.filter((c) => c.cmd === 'op_discard').map((c) => c.args.opId);

test('a confirmed plan that ends after a lock took its screen: its failure shows after the unlock', async () => {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <LockGate>
          <ToastProvider>
            <ViewFrame>
              <OpenConfirm />
            </ViewFrame>
            <EndedAwayNotices />
            <ToastViewport />
          </ToastProvider>
        </LockGate>
      </PlatformContext>
    </QueryClientProvider>,
  );
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Rename' }));
  expect(calls.map((c) => c.cmd)).toContain('op_execute');

  await changed(true);
  expect(screen.queryByRole('dialog', { name: 'Rename prod-eu?' })).toBeNull();
  ended = true; // it fails in Rust while the app is locked

  await changed(false);
  expect(await screen.findByText('Rename prod-eu failed: cx22 is sold out in nbg1')).toBeDefined();
  expect(discarded()).toEqual([900]);
  expect(endedAwaySnapshot()).toEqual([]);
});

/**
 * The app as the tests above render it: the lock gate, a shell with the confirm, the notices;
 * `schedule`, the toasts' clock, the test's own where it is given.
 */
function renderApp(schedule?: Schedule) {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <LockGate>
          <ToastProvider {...(schedule !== undefined && { schedule })}>
            <ViewFrame>
              <OpenConfirm />
            </ViewFrame>
            <EndedAwayNotices />
            <ToastViewport />
          </ToastProvider>
        </LockGate>
      </PlatformContext>
    </QueryClientProvider>,
  );
}

test('D.4a F18: a confirmed plan that completes after a lock took its screen says so after the unlock', async () => {
  renderApp();
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Rename' }));
  await changed(true);
  ended = true; // Rust judged the typed secret and ran the change while the app was locked
  endedDone = true;
  await changed(false);
  expect(await screen.findByText('Rename prod-eu: done.')).toBeDefined();
  expect(discarded()).toEqual([900]);
});

test('D.4a F18: a confirm the lock stopped before it began says after the unlock that it was not started', async () => {
  // The OS prompt is open when the lock lands: Rust closes it and answers auth_cancelled.
  let refuse = () => {};
  execute = () =>
    new Promise((_, reject) => {
      refuse = () =>
        reject({
          code: DESKTOP_ERROR_CODES.AUTH_CANCELLED,
          message: 'authentication was cancelled',
          help: null,
          causes: [],
          fields: {},
        });
    });
  renderApp();
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Rename' }));
  await changed(true);
  await act(async () => {
    refuse();
    await new Promise((r) => setTimeout(r, 0));
  });
  expect(screen.queryByText(/was not started/)).toBeNull(); // nothing shows while locked
  await changed(false);
  expect(
    await screen.findByText('Rename prod-eu was not started: AppRafter locked before it began.'),
  ).toBeDefined();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('ends kept away show one at a time, each discarded once shown', async () => {
  keepEndedAway({ opId: 31, text: 'Add target lab: done.', failed: false });
  keepEndedAway({ opId: 32, text: 'Rename prod failed: taken', failed: true });
  render(
    <ToastProvider>
      <EndedAwayNotices spacingMs={0} />
      <ToastViewport />
    </ToastProvider>,
  );
  expect(await screen.findByText('Add target lab: done.')).toBeDefined();
  expect(await screen.findByText('Rename prod failed: taken')).toBeDefined();
  expect(discarded()).toEqual([31, 32]);
  expect(endedAwaySnapshot()).toEqual([]);
});

test('each notice is given its time: the next waits for the spacing, and so does its discard', async () => {
  keepEndedAway({ opId: 41, text: 'Add target lab: done.', failed: false });
  keepEndedAway({ opId: 42, text: 'Rename prod failed: taken', failed: true });
  render(
    <ToastProvider schedule={() => () => {}}>
      <EndedAwayNotices spacingMs={300} />
      <ToastViewport />
    </ToastProvider>,
  );
  expect(await screen.findByText('Add target lab: done.')).toBeDefined();
  // Within the spacing: the first still shows, the second waits, unshown and not discarded.
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(screen.queryByText('Rename prod failed: taken') === null).toBe(true);
  expect(screen.queryByText('Add target lab: done.') === null).toBe(false);
  expect(discarded()).toEqual([41]);
  expect(await screen.findByText('Rename prod failed: taken', {}, { timeout: 2000 })).toBeDefined();
  expect(discarded()).toEqual([41, 42]);
});

test('a note with no operation of its own shows like an end, and nothing is discarded for it', async () => {
  // A read's warnings its screen went before it showed (the doctor's): their operation is gone.
  keepEndedAway({ opId: null, text: 'Doctor · prod: copies left on disk', failed: true });
  keepEndedAway({ opId: null, text: 'Doctor · prod: another', failed: true });
  keepEndedAway({ opId: 51, text: 'Add target lab: done.', failed: false });
  render(
    <ToastProvider>
      <EndedAwayNotices spacingMs={0} />
      <ToastViewport />
    </ToastProvider>,
  );
  expect(await screen.findByText('Doctor · prod: copies left on disk')).toBeDefined();
  expect(await screen.findByText('Doctor · prod: another')).toBeDefined();
  expect(await screen.findByText('Add target lab: done.')).toBeDefined();
  expect(discarded()).toEqual([51]);
  expect(endedAwaySnapshot()).toEqual([]);
});

/** A toast clock the test runs by hand: each scheduled end, and whether it was cancelled. */
function manualClock() {
  const jobs: { run: () => void; cancelled: boolean }[] = [];
  const schedule: Schedule = (run) => {
    const job = { run, cancelled: false };
    jobs.push(job);
    return () => {
      job.cancelled = true;
    };
  };
  return { jobs, schedule };
}

// D.4a N5 (the owner walk's D-7 (a), presses at 0.72 s and 1.51 s): a change that ended a moment
// before the lock showed its own toast only before it, and after the unlock nothing said how it
// ended. Its toast, still shown when the app locks, now shows again after the unlock.
test('D.4a N5: a change that ended a moment before the lock says so again after the unlock', async () => {
  const { jobs, schedule } = manualClock();
  endsAtOnce = true; // it ends as soon as it runs, its page still shown
  renderApp(schedule);
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Rename' }));
  expect(await screen.findByText(RENAMED)).toBeDefined();
  const shown = jobs.filter((j) => !j.cancelled);
  expect(shown.length).toBe(1); // its TOAST_MS not over: still shown when the app locks
  await changed(true);
  expect(screen.queryByText(RENAMED)).toBeNull();
  // Kept once, in the page's words; startPlan kept nothing (its page took the end).
  expect(endedAwaySnapshot()).toEqual([{ opId: null, text: RENAMED, failed: false }]);
  await changed(false);
  expect(await screen.findByText(RENAMED)).toBeDefined();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('D.4a N5: a change whose toast had gone before the lock says nothing more after it', async () => {
  const { jobs, schedule } = manualClock();
  endsAtOnce = true;
  renderApp(schedule);
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Rename' }));
  expect(await screen.findByText(RENAMED)).toBeDefined();
  act(() => {
    for (const job of jobs.filter((j) => !j.cancelled)) job.run(); // its TOAST_MS are over
  });
  expect(screen.queryByText(RENAMED)).toBeNull();
  await changed(true);
  expect(endedAwaySnapshot()).toEqual([]);
});

test('D.4a N5: a notice the app shows after an unlock is an end too: a lock while it shows keeps it again', async () => {
  const { schedule } = manualClock();
  keepEndedAway({ opId: 61, text: 'Rename prod failed: taken', failed: true });
  renderApp(schedule);
  expect(await screen.findByText('Rename prod failed: taken')).toBeDefined();
  expect(discarded()).toEqual([61]);
  await changed(true);
  // Kept again as what it is, a failure; its operation was discarded when it first showed.
  expect(endedAwaySnapshot()).toEqual([
    { opId: null, text: 'Rename prod failed: taken', failed: true },
  ]);
  await changed(false);
  expect(await screen.findByText('Rename prod failed: taken')).toBeDefined();
  expect(discarded()).toEqual([61]);
});
