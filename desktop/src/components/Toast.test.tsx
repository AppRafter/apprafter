// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, mock, test } from 'bun:test';
import { act, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { TOAST_MS, ToastProvider, ToastViewport, useToast } from './Toast';

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
