// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, expect, mock, test } from 'bun:test';
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { endedAwaySnapshot, resetEndedAway } from '../ipc/away';
import { sessionLocked } from '../ipc/lifecycle';
import { TOAST_MS, type ToastMessage, ToastProvider, ToastViewport, useToast } from './Toast';

afterEach(() => {
  resetEndedAway();
});

/** A scheduler the test runs by hand: what was scheduled, after how long, and its cancel. */
function manualSchedule() {
  const jobs: { run: () => void; ms: number; cancelled: boolean }[] = [];
  const schedule = (run: () => void, ms: number) => {
    const job = { run, ms, cancelled: false };
    jobs.push(job);
    return () => {
      job.cancelled = true;
    };
  };
  return { jobs, schedule };
}

function Trigger() {
  const toast = useToast();
  return (
    <>
      <button type="button" onClick={() => toast({ message: 'Settings saved' })}>
        First
      </button>
      <button type="button" onClick={() => toast({ message: 'Theme changed' })}>
        Second
      </button>
    </>
  );
}

function setup() {
  const { jobs, schedule } = manualSchedule();
  render(
    <ToastProvider schedule={schedule}>
      <Trigger />
      <ToastViewport />
    </ToastProvider>,
  );
  return { user: userEvent.setup(), jobs };
}

test('a toast is announced politely in the one slot', async () => {
  const { user } = setup();
  const region = screen.getByRole('status');
  expect(region.getAttribute('aria-live')).toBe('polite');
  expect(region.textContent).toBe('');
  await user.click(screen.getByRole('button', { name: 'First' }));
  expect(region.textContent).toBe('Settings saved');
});

test('the newest replaces the one shown, and its predecessor timer is cancelled', async () => {
  const { user, jobs } = setup();
  await user.click(screen.getByRole('button', { name: 'First' }));
  await user.click(screen.getByRole('button', { name: 'Second' }));
  expect(screen.getByRole('status').textContent).toBe('Theme changed');
  expect(jobs.map((j) => j.cancelled)).toEqual([true, false]);
});

test(`it goes after ${TOAST_MS} ms`, async () => {
  const { user, jobs } = setup();
  await user.click(screen.getByRole('button', { name: 'First' }));
  await user.click(screen.getByRole('button', { name: 'Second' }));
  expect(TOAST_MS).toBe(3400);
  expect(jobs.map((j) => j.ms)).toEqual([3400, 3400]);
  // The replaced toast's timer firing late changes nothing.
  act(() => jobs[0]?.run());
  expect(screen.getByRole('status').textContent).toBe('Theme changed');
  act(() => jobs[1]?.run());
  expect(screen.getByRole('status').textContent).toBe('');
});

test('useToast outside the provider is a programming error', () => {
  const quiet = mock(() => {});
  const original = console.error;
  console.error = quiet;
  try {
    expect(() => render(<Trigger />)).toThrow('ToastProvider');
  } finally {
    console.error = original;
  }
});

// D.4a N5: the lock unmounts the shell and its toast slot. A toast that says how a change ended
// and is still in the slot when the app locks is kept for after the unlock (ipc/away.ts); one
// whose TOAST_MS is over, or that a newer toast replaced, is not. The clock is the test's own
// schedule: nothing waits on real time.

/** Buttons that show `toasts`, one each, named by their message. */
function Show({ toasts }: { toasts: readonly ToastMessage[] }) {
  const toast = useToast();
  return (
    <>
      {toasts.map((t) => (
        <button key={t.message} type="button" onClick={() => toast(t)}>
          {t.message}
        </button>
      ))}
    </>
  );
}

const DONE: ToastMessage = { message: 'Secrets are in files again.', end: 'done' };
const FAILED: ToastMessage = { message: 'Protecting was cancelled.', end: 'failed' };
const COPIED: ToastMessage = { message: 'Copied. The clipboard is cleared in 60 s.' };
const NOT_KEPT: ToastMessage = {
  message: 'the vault does not hold what was written',
  code: 'apprafter::secrets::files_not_kept',
  end: 'failed',
};

function slot() {
  const { jobs, schedule } = manualSchedule();
  render(
    <ToastProvider schedule={schedule}>
      <Show toasts={[DONE, FAILED, COPIED, NOT_KEPT]} />
      <ToastViewport />
    </ToastProvider>,
  );
  const show = (t: ToastMessage) =>
    userEvent.setup().click(screen.getByRole('button', { name: t.message }));
  const lock = () =>
    act(() => {
      sessionLocked();
    });
  return { jobs, show, lock };
}

test('D.4a N5: an end still shown when the app locks is kept for after the unlock', async () => {
  const { jobs, show, lock } = slot();
  await show(DONE);
  expect(jobs.map((j) => [j.ms, j.cancelled])).toEqual([[TOAST_MS, false]]); // still shown
  lock();
  expect(endedAwaySnapshot()).toEqual([
    { opId: null, text: 'Secrets are in files again.', failed: false },
  ]);
});

test('D.4a N5: a failure or a cancel still shown is kept as one', async () => {
  const { show, lock } = slot();
  await show(FAILED);
  lock();
  expect(endedAwaySnapshot()).toEqual([
    { opId: null, text: 'Protecting was cancelled.', failed: true },
  ]);
});

test(`D.4a N5: an end whose ${TOAST_MS} ms are over is not kept`, async () => {
  const { jobs, show, lock } = slot();
  await show(DONE);
  act(() => jobs[0]?.run()); // its time is over: it went
  expect(screen.getByRole('status').textContent).toBe('');
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('D.4a N5: an end a newer toast replaced is not kept, nor is a toast that is no end', async () => {
  const { show, lock } = slot();
  await show(DONE);
  await show(COPIED); // the copy took its place
  expect(screen.getByRole('status').textContent).toBe(COPIED.message);
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('D.4a N5: a newer end replaces the one kept, and the lock keeps only it, once', async () => {
  const { show, lock } = slot();
  await show(FAILED);
  await show(DONE);
  lock();
  lock(); // the next session's lock finds nothing of the slot the first lock took
  expect(endedAwaySnapshot()).toEqual([
    { opId: null, text: 'Secrets are in files again.', failed: false },
  ]);
});

test('D.4a N5: a slot that goes without a lock (the quit) keeps nothing, then or at a later lock', async () => {
  const { show, lock } = slot();
  await show(DONE);
  cleanup();
  lock();
  expect(endedAwaySnapshot()).toEqual([]);
});

// WI-483: a failure's toast says its code under the message, as the ErrorPanel does, and a lock
// that keeps the toast keeps the code with it.
test('WI-483: a failure’s code shows under its message, and a lock keeps it with the message', async () => {
  const { show, lock } = slot();
  await show(NOT_KEPT);
  const code = screen.getByText('apprafter::secrets::files_not_kept');
  expect(code.className).toBe('toast-code');
  expect(screen.getByRole('status').textContent).toBe(
    'the vault does not hold what was writtenapprafter::secrets::files_not_kept',
  );
  lock();
  expect(endedAwaySnapshot()).toEqual([
    {
      opId: null,
      text: 'the vault does not hold what was written',
      failed: true,
      code: 'apprafter::secrets::files_not_kept',
    },
  ]);
});

test('WI-483: a toast with no code shows none', async () => {
  const { show } = slot();
  await show(FAILED);
  expect(document.querySelector('.toast-code')).toBeNull();
  expect(screen.getByRole('status').textContent).toBe(FAILED.message);
});
