// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { Channel } from '@tauri-apps/api/core';
import { clearMocks } from '@tauri-apps/api/mocks';
import { screen, waitFor } from '@testing-library/react';
import * as api from '../ipc/api';
import { CORE_ERROR_CODES } from '../ipc/generated/core-errors';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { OpEvent } from '../ipc/generated/OpEvent';
import type { PlanView } from '../ipc/generated/PlanView';
import type { UiError } from '../ipc/generated/UiError';
import { installMockIpc, mockOps } from '../ipc/mock';
import type { MockResult } from '../ipc/mock/ops';
import { resetOperations } from '../ipc/operations';
import { renderScreen } from '../test/screens';
import { settleIpc } from '../test/settle';
import { PlanConfirm } from './PlanConfirm';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  await settleIpc();
  resetOperations();
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
