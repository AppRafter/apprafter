// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { Channel } from '@tauri-apps/api/core';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import * as api from '../ipc/api';
import { endedAwaySnapshot, resetEndedAway } from '../ipc/away';
import { CORE_ERROR_CODES } from '../ipc/generated/core-errors';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { OpEvent } from '../ipc/generated/OpEvent';
import type { PlanView } from '../ipc/generated/PlanView';
import type { UiError } from '../ipc/generated/UiError';
import { holdPlan } from '../ipc/heldPlans';
import { resetLifecycle } from '../ipc/lifecycle';
import { installMockIpc, mockOps } from '../ipc/mock';
import { createMockOps, type MockResult } from '../ipc/mock/ops';
import { resetOperations } from '../ipc/operations';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo } from '../test/fixtures';
import { planParts } from '../test/flows';
import { completed, failed, installHarness, uiError as uiErrorOf } from '../test/ipc';
import { renderScreen } from '../test/screens';
import { settleIpc } from '../test/settle';
import { tabHost } from '../test/tab';
import { PlanConfirm } from './PlanConfirm';
import { ToastProvider } from './Toast';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  resetEndedAway();
  resetLifecycle();
  clearMocks();
});

const CHANGES: PlanView['changes'] = [
  { kind: 'Target', object: 'prod', action: 'rename', detail: 'prod → lab' },
];
const plan = (cls: 'bounded' | 'destructive', end: () => MockResult): PlanView =>
  mockOps().registerPlan({ class: cls, title: 't', changes: CHANGES, target: 'prod' }, { end });
const handlers = () => ({ onDone: mock(), onFailed: mock(), onClose: mock() });

test('a bounded plan lists its changes; the confirm runs it, hands over the result and closes', async () => {
  const h = handlers();
  const view = plan('bounded', () => ({ result: { from: 'prod', to: 'lab' } }));
  const user = renderScreen(
    <PlanConfirm view={view} title="Rename prod?" confirmLabel="Rename" auth={null} {...h} />,
  );
  expect(screen.getByText(/prod → lab/)).toBeDefined();
  await user.click(screen.getByRole('button', { name: 'Rename' }));
  await waitFor(() => expect(h.onDone).toHaveBeenCalledWith({ from: 'prod', to: 'lab' }));
  expect(h.onClose).toHaveBeenCalledTimes(1);
  expect(h.onFailed).not.toHaveBeenCalled();
});

test('a destructive plan shows the plan and waits for the typed name', async () => {
  const h = handlers();
  const view = plan('destructive', () => ({ result: { name: 'prod' } }));
  const user = renderScreen(
    <PlanConfirm
      view={view}
      title="Remove target prod?"
      confirmLabel="Remove target"
      requireText="prod"
      auth={null}
      {...h}
    />,
  );
  expect(screen.getByText(/prod → lab/)).toBeDefined();
  const remove = screen.getByRole('button', { name: 'Remove target' }) as HTMLButtonElement;
  expect(remove.disabled).toBe(true);
  await user.type(screen.getByLabelText(/to confirm/), 'pro');
  expect(remove.disabled).toBe(true);
  await user.type(screen.getByLabelText(/to confirm/), 'd');
  expect(remove.disabled).toBe(false);
  await user.click(remove);
  await waitFor(() => expect(h.onDone).toHaveBeenCalledWith({ name: 'prod' }));
});

test('Cancel on a plan that never ran discards it in Rust', async () => {
  const h = handlers();
  const view = plan('bounded', () => ({ result: null }));
  const user = renderScreen(
    <PlanConfirm view={view} title="Rename prod?" confirmLabel="Rename" auth={null} {...h} />,
  );
  await user.click(screen.getByRole('button', { name: 'Cancel' }));
  expect(h.onClose).toHaveBeenCalledTimes(1);
  await new Promise((resolve) => setTimeout(resolve, 0)); // the discard's IPC round trip
  const refused = await api.opExecute(view.opId, new Channel<OpEvent>()).catch((e: unknown) => e);
  expect((refused as api.IpcError).error.code).toBe(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND);
  expect(h.onDone).not.toHaveBeenCalled();
});

test('a run that fails hands its UiError to onFailed, and the dialog closes', async () => {
  const h = handlers();
  const taken: UiError = {
    code: CORE_ERROR_CODES.TARGET_EXISTS,
    message: 'target `lab` already exists',
    help: null,
    causes: [],
    fields: { name: 'lab' },
  };
  const view = plan('bounded', () => ({ error: taken }));
  const user = renderScreen(
    <PlanConfirm view={view} title="Rename prod?" confirmLabel="Rename" auth={null} {...h} />,
  );
  await user.click(screen.getByRole('button', { name: 'Rename' }));
  await waitFor(() => expect(h.onFailed).toHaveBeenCalledWith(taken));
  expect(h.onDone).not.toHaveBeenCalled();
  expect(h.onClose).toHaveBeenCalledTimes(1);
});

test('a cancelled run ends like a failed one: OP_CANCELLED to onFailed', async () => {
  clearMocks();
  installMockIpc({ opDelayMs: 10_000 });
  await api.unlock();
  const h = handlers();
  const view = plan('bounded', () => ({ result: null }));
  const user = renderScreen(
    <PlanConfirm view={view} title="Rename prod?" confirmLabel="Rename" auth={null} {...h} />,
  );
  await user.click(screen.getByRole('button', { name: 'Rename' }));
  await waitFor(async () =>
    expect((await api.opList()).some((s) => s.opId === view.opId)).toBe(true),
  );
  await api.opCancel(view.opId);
  await waitFor(() =>
    expect(h.onFailed).toHaveBeenCalledWith(
      expect.objectContaining({ code: CORE_ERROR_CODES.OP_CANCELLED }),
    ),
  );
  expect(h.onDone).not.toHaveBeenCalled();
});

test('a reversible plan has no dialog: it is refused', () => {
  const view = mockOps().registerPlan(
    { class: 'reversible', title: 't', changes: [], target: 'prod' },
    { end: () => ({ result: null }) },
  );
  const h = handlers();
  const quiet = console.error;
  console.error = () => {};
  try {
    expect(() =>
      renderScreen(
        <PlanConfirm view={view} title="Use prod?" confirmLabel="Use" auth={null} {...h} />,
      ),
    ).toThrow(/reversible/);
  } finally {
    console.error = quiet;
  }
});

/**
 * A confirm in a tab (test/tab.tsx), its plan run waiting on the IPC harness until `end(event)`;
 * the tab can be hidden, shown and closed as the Shell does.
 */
async function confirmedInTab() {
  clearMocks();
  const h = installHarness();
  let channel: { id: number } | null = null;
  h.answer('op_execute', ({ onEvent }: Record<string, unknown>) => {
    channel = onEvent as { id: number };
    return 2;
  });
  const view: PlanView = { ...planParts({ title: 'Rename prod' }), opId: h.newOperation([]) };
  const handles = handlers();
  const tab = tabHost('prod');
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <ToastProvider>
          <tab.Tab>
            <PlanConfirm
              view={view}
              title="Rename prod?"
              confirmLabel="Rename"
              auth={null}
              {...handles}
            />
          </tab.Tab>
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  await userEvent.setup().click(screen.getByRole('button', { name: 'Rename' }));
  await waitFor(() => expect(channel).not.toBeNull());
  const internals = (
    window as unknown as { __TAURI_INTERNALS__: { runCallback(id: number, data: unknown): void } }
  ).__TAURI_INTERNALS__;
  const end = async (message: OpEvent) => {
    await act(async () => {
      internals.runCallback(channel?.id ?? -1, { index: 0, message });
      await settleIpc();
    });
  };
  return { h, view, handles, tab, end };
}

test('a plan that ends while its tab is hidden: onDone once the tab is back, nothing kept away', async () => {
  const { handles, tab, end } = await confirmedInTab();
  tab.hide();
  await end(completed({ from: 'prod', to: 'lab' }));
  expect(handles.onDone).not.toHaveBeenCalled();
  tab.show();
  await waitFor(() => expect(handles.onDone).toHaveBeenCalledWith({ from: 'prod', to: 'lab' }));
  expect(handles.onDone).toHaveBeenCalledTimes(1);
  await waitFor(() => expect(handles.onClose).toHaveBeenCalledTimes(1));
  expect(endedAwaySnapshot()).toEqual([]);
});

test('a plan that fails while its tab is hidden: onFailed once the tab is back', async () => {
  const { handles, tab, end } = await confirmedInTab();
  tab.hide();
  await end(failed(uiErrorOf('apprafter::target::busy', 'prod is busy')));
  tab.show();
  await waitFor(() =>
    expect(handles.onFailed.mock.calls.map(([e]) => (e as UiError).message)).toEqual([
      'prod is busy',
    ]),
  );
  expect(handles.onDone).not.toHaveBeenCalled();
  expect(endedAwaySnapshot()).toEqual([]);
});

test('its tab closed before it was back, the end it got while hidden shows at the app level', async () => {
  const { view, handles, tab, end } = await confirmedInTab();
  tab.hide();
  await end(completed({ from: 'prod', to: 'lab' }));
  tab.close();
  expect(endedAwaySnapshot()).toEqual([
    { opId: view.opId, text: 'Rename prod: done.', failed: false },
  ]);
  expect(handles.onDone).not.toHaveBeenCalled();
});

test('its tab closed while the plan runs: the end shows at the app level', async () => {
  const { view, handles, tab, end } = await confirmedInTab();
  tab.close();
  await end(completed({ from: 'prod', to: 'lab' }));
  expect(endedAwaySnapshot()).toEqual([
    { opId: view.opId, text: 'Rename prod: done.', failed: false },
  ]);
  expect(handles.onDone).not.toHaveBeenCalled();
});

/**
 * Rust leaves a plan alone while its OS prompt is open (OperationManager::discard), and a
 * refusal it may retry (a wrong password, the back-off, the other way to ask) puts it back to
 * wait. So a tab closed during the prompt sends its hold's discard to a plan Rust keeps: the
 * confirm discards it once the refusal comes, as no retry can come from a screen that is gone.
 */
test('its tab closed while the OS prompt is open, then refused: the plan is discarded after the refusal', async () => {
  clearMocks();
  let refuse: (reason: unknown) => void = () => {};
  const engine = createMockOps({
    delayMs: 0,
    gesture: () =>
      new Promise<void>((_, reject) => {
        refuse = reject;
      }),
  });
  const calls: { readonly cmd: string; readonly args: Record<string, unknown> }[] = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
    const handler = (engine.handlers as Record<string, (a: unknown) => unknown>)[cmd];
    return handler === undefined ? null : handler(args);
  });
  const view = engine.registerPlan(
    { class: 'destructive', title: 'Remove prod', changes: CHANGES, target: 'prod' },
    { end: () => ({ result: { name: 'prod' } }) },
  );
  const discards = () => calls.filter((c) => c.cmd === 'op_discard').map((c) => c.args.opId);
  const tab = tabHost('prod');
  holdPlan(tab.scope, view.opId); // as the screen that opened the confirm does
  const handles = handlers();
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <ToastProvider>
          <tab.Tab>
            <PlanConfirm
              view={view}
              title="Remove prod?"
              confirmLabel="Remove"
              auth={null}
              {...handles}
            />
          </tab.Tab>
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  await userEvent.setup().click(screen.getByRole('button', { name: 'Remove' }));
  await act(async () => {
    await settleIpc();
  });
  expect(calls.filter((c) => c.cmd === 'op_execute')).toHaveLength(1);
  // The owner closes the tab while the OS prompt is up: the hold discards, Rust ignores it.
  tab.close();
  await act(async () => {
    await settleIpc();
  });
  expect(discards()).toEqual([view.opId]);
  // The prompt answers: not verified. Rust would keep the plan for another try.
  await act(async () => {
    refuse({
      code: DESKTOP_ERROR_CODES.AUTH_FAILED,
      message: 'Authentication failed',
      help: null,
      causes: [],
      fields: { exhausted: false, retryInMs: null },
    });
    await settleIpc();
  });
  expect(discards()).toEqual([view.opId, view.opId]);
  // Nothing waits in Rust: a second execute finds no plan.
  const again = api.opExecute(view.opId, new Channel<OpEvent>());
  await expect(again).rejects.toMatchObject({
    error: { code: DESKTOP_ERROR_CODES.PLAN_NOT_FOUND },
  });
  expect(handles.onDone).not.toHaveBeenCalled();
  expect(handles.onFailed).not.toHaveBeenCalled();
});
