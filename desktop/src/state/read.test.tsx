// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, spyOn, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, render, renderHook, screen, waitFor } from '@testing-library/react';
import { Activity, type ReactNode, useEffect } from 'react';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import {
  newScope,
  resetLifecycle,
  type Scope,
  sessionLocked,
  sessionScope,
} from '../ipc/lifecycle';
import { resetOperations } from '../ipc/operations';
import { cancelled, completed, failed, type Harness, installHarness, uiError } from '../test/ipc';
import { settleIpc } from '../test/settle';
import { useRead } from './read';
import { ScopeContext } from './scope';

let h: Harness;
beforeEach(() => {
  h = installHarness();
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetLifecycle();
  resetOperations();
  clearMocks();
});

test('run goes idle → running (no id) → running (id) → done', async () => {
  h.operation(7, [completed({ ok: true })]);
  let release = () => {};
  const started = new Promise<number>((resolve) => {
    release = () => resolve(7);
  });
  const { result } = renderHook(() => useRead<{ ok: boolean }>());
  expect(result.current.state).toEqual({ status: 'idle' });
  let done: Promise<{ ok: boolean } | null> = Promise.resolve(null);
  act(() => {
    done = result.current.run(() => started);
  });
  expect(result.current.state).toEqual({ status: 'running', opId: null });
  await act(async () => {
    release();
    await done;
  });
  expect(result.current.state).toEqual({ status: 'done', data: { ok: true } });
});

test('a failed run ends failed with the UiError', async () => {
  h.operation(8, [failed(uiError('apprafter::target::token_rejected', 'rejected'))]);
  const { result } = renderHook(() => useRead<unknown>());
  await act(async () => {
    await result.current.run(async () => 8);
  });
  expect(result.current.state).toEqual({
    status: 'failed',
    error: uiError('apprafter::target::token_rejected', 'rejected'),
  });
});

test('cancel while running calls op_cancel with the op id', async () => {
  h.operation(9, []); // keeps running (the harness cannot push onto a live subscription)
  const { result } = renderHook(() => useRead<unknown>());
  act(() => {
    void result.current.run(async () => 9);
  });
  await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId: 9 }));
  act(() => result.current.cancel());
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 9 }]);
});

test("a second run cancels the first, and the first one's late end changes nothing", async () => {
  h.operation(10, []);
  h.operation(11, [completed('second')]);
  const { result } = renderHook(() => useRead<string>());
  act(() => {
    void result.current.run(async () => 10);
  });
  await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId: 10 }));
  await act(async () => {
    await result.current.run(async () => 11);
  });
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 10 }]);
  expect(result.current.state).toEqual({ status: 'done', data: 'second' });
});

/** useRead in the screen `scope` stands for (the session's by default). */
function renderRead<T>(scope: Scope = sessionScope()) {
  return renderHook(() => useRead<T>(), {
    wrapper: ({ children }: { children: ReactNode }) => (
      <ScopeContext value={scope}>{children}</ScopeContext>
    ),
  });
}

test('an unmount cancels nothing: an Activity hide runs the same cleanups', async () => {
  const opId = h.newOperation([]);
  const { result, unmount } = renderRead<unknown>();
  act(() => {
    void result.current.run(async () => opId);
  });
  await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId }));
  unmount();
  await settleIpc();
  expect(h.of('op_cancel')).toEqual([]);
});

test('its screen going cancels what runs: the dialog or the tab closed, or the lock', async () => {
  for (const end of ['screen', 'view', 'lock'] as const) {
    const view = newScope(sessionScope());
    const dialog = newScope(view.scope);
    const opId = h.newOperation([]);
    const { result } = renderRead<unknown>(dialog.scope);
    act(() => {
      void result.current.run(async () => opId);
    });
    await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId }));
    if (end === 'screen') dialog.end();
    else if (end === 'view') view.end();
    else sessionLocked();
    expect({ end, cancels: h.of('op_cancel').map((c) => c.args.opId) }).toEqual({
      end,
      cancels: [opId],
    });
    h.calls.length = 0;
  }
});

test('an op_cancel refused as locked logs nothing', async () => {
  const spy = spyOn(console, 'error').mockImplementation(() => {});
  const opId = h.newOperation([]);
  h.answer('op_cancel', () => Promise.reject(uiError(DESKTOP_ERROR_CODES.LOCKED)));
  const { result } = renderRead<unknown>();
  act(() => {
    void result.current.run(async () => opId);
  });
  await waitFor(() => expect(result.current.state).toEqual({ status: 'running', opId }));
  sessionLocked();
  await waitFor(() => expect(h.of('op_cancel')).toHaveLength(1));
  await settleIpc();
  expect(spy).not.toHaveBeenCalled();
  spy.mockRestore();
});

test('hidden in an Activity, a read runs on, and its end shows when it is shown again', async () => {
  let release = (_opId: number) => {};
  const opId = h.newOperation([completed('found')]);
  let starts = 0;
  // As the screens do: a read started while idle, so a re-show starts nothing new.
  function Reader() {
    const read = useRead<string>();
    const { run } = read;
    const idle = read.state.status === 'idle';
    useEffect(() => {
      if (!idle) return;
      starts += 1;
      void run(
        () =>
          new Promise<number>((resolve) => {
            release = resolve;
          }),
      );
    }, [idle, run]);
    return <p>{read.state.status === 'done' ? read.state.data : read.state.status}</p>;
  }
  function Host({ shown }: { shown: boolean }) {
    return (
      <Activity mode={shown ? 'visible' : 'hidden'}>
        <Reader />
      </Activity>
    );
  }
  const view = render(<Host shown />);
  expect(await screen.findByText('running')).toBeDefined();
  view.rerender(<Host shown={false} />);
  await act(async () => {
    release(opId);
    await settleIpc();
  });
  expect(h.of('op_cancel')).toEqual([]);
  view.rerender(<Host shown />);
  expect(await screen.findByText('found')).toBeDefined();
  expect(starts).toBe(1);
});

/** A start that answers only when the test releases it. */
function heldStart(opId: number) {
  let release = () => {};
  const started = new Promise<number>((resolve) => {
    release = () => resolve(opId);
  });
  return { start: () => started, release };
}

test("a superseded run's late end changes nothing; its result goes to onUnused", async () => {
  h.operation(10, [completed('first')]);
  h.operation(11, [completed('second')]);
  const first = heldStart(10);
  const unused = mock();
  const { result } = renderHook(() => useRead<string>());
  let firstDone: Promise<string | null> = Promise.resolve(null);
  act(() => {
    firstDone = result.current.run(first.start, unused);
  });
  await act(async () => {
    await result.current.run(async () => 11);
  });
  expect(result.current.state).toEqual({ status: 'done', data: 'second' });
  let late: string | null = 'not yet';
  await act(async () => {
    first.release();
    late = await firstDone;
  });
  expect(late).toBeNull();
  // Stopped before Rust answered: cancelled once its id arrived.
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 10 }]);
  expect(unused.mock.calls).toEqual([['first']]);
  expect(result.current.state).toEqual({ status: 'done', data: 'second' });
});

test('a superseded run that ends cancelled does not overwrite the run that replaced it', async () => {
  h.operation(10, [cancelled()]);
  h.operation(11, [completed('second')]);
  const first = heldStart(10);
  const { result } = renderHook(() => useRead<string>());
  let firstDone: Promise<string | null> = Promise.resolve(null);
  act(() => {
    firstDone = result.current.run(first.start);
  });
  await act(async () => {
    await result.current.run(async () => 11);
  });
  await act(async () => {
    first.release();
    await firstDone;
  });
  expect(result.current.state).toEqual({ status: 'done', data: 'second' });
});

test('a result that lands after its screen went is not used: run resolves null, onUnused gets it', async () => {
  const opId = h.newOperation([completed('late')]);
  const held = heldStart(opId);
  const unused = mock();
  const dialog = newScope(sessionScope());
  const { result, unmount } = renderRead<string>(dialog.scope);
  let done: Promise<string | null> = Promise.resolve(null);
  act(() => {
    done = result.current.run(held.start, unused);
  });
  dialog.end();
  unmount();
  held.release();
  expect(await done).toBeNull();
  expect(unused.mock.calls).toEqual([['late']]);
  // Stopped before Rust answered: cancelled once its id arrived.
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId }]);
});

test('once its screen is gone a run starts nothing', async () => {
  const dialog = newScope(sessionScope());
  const { result } = renderRead<string>(dialog.scope);
  dialog.end();
  const start = mock(async () => 13);
  expect(await result.current.run(start)).toBeNull();
  expect(start).not.toHaveBeenCalled();
});
