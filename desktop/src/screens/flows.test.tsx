// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The D.3 flows, opened as the views open them, on D.3d's mock engine.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, screen, within } from '@testing-library/react';
import * as api from '../ipc/api';
import { resetLifecycle } from '../ipc/lifecycle';
import { installMockIpc } from '../ipc/mock';
import { resetOperations } from '../ipc/operations';
import { targetReport } from '../test/fixtures';
import { doctorReport } from '../test/flows';
import { completed, installHarness, uiError } from '../test/ipc';
import { renderScreen } from '../test/screens';
import { settleIpc } from '../test/settle';
import { type TargetFlows, useTargetFlows } from './flows';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetLifecycle();
  resetOperations();
  clearMocks();
});

/** A probe inside renderScreen; `flows()` is what `useTargetFlows` returned on its last render. */
function probe(): { flows: () => TargetFlows; user: ReturnType<typeof renderScreen> } {
  const seen: { flows: TargetFlows | null } = { flows: null };
  function Probe() {
    seen.flows = useTargetFlows();
    return null;
  }
  const user = renderScreen(<Probe />);
  return {
    user,
    flows: () => {
      if (seen.flows === null) throw new Error('the probe did not render');
      return seen.flows;
    },
  };
}

/** The overlay a dialog sits in: the app's own (outside the view) or the view's. */
const hostOf = (dialog: HTMLElement) => (dialog.closest('.view') === null ? 'app' : 'view');

test('addTarget opens the wizard, over the views', async () => {
  const { flows } = probe();
  act(() => flows().addTarget());
  const wizard = await screen.findByRole('dialog', { name: 'Add target' });
  expect(hostOf(wizard)).toBe('app');
});

test('doctor opens Doctor · <target>, over the views', async () => {
  const { flows } = probe();
  act(() => flows().doctor('prod-eu'));
  const doctor = await screen.findByRole('dialog', { name: 'Doctor · prod-eu' });
  expect(hostOf(doctor)).toBe('app');
});

test('changeMachine opens Change machine · <target>, in its view', async () => {
  const { flows } = probe();
  act(() => flows().changeMachine('staging', { region: 'nbg1', serverType: 'cx22' }));
  const dialog = await screen.findByRole('dialog', { name: 'Change machine · staging' });
  expect(hostOf(dialog)).toBe('view');
});

test('toolchain opens the panel, over the views', async () => {
  const { flows } = probe();
  act(() => flows().toolchain());
  expect(hostOf(await screen.findByRole('dialog', { name: 'Toolchain' }))).toBe('app');
});

test('errorAction runs the actions these flows own, and only those', async () => {
  const { flows } = probe();
  let owned = true;
  act(() => {
    owned = flows().errorAction({ kind: 'renew-token' });
  });
  expect(owned).toBe(false);
  expect(screen.queryByRole('dialog')).toBeNull();
  act(() => {
    owned = flows().errorAction({ kind: 'toolchain' });
  });
  expect(owned).toBe(true);
  expect(await screen.findByRole('dialog', { name: 'Toolchain' })).toBeDefined();
});

test("the doctor's Add target closes the doctor, then opens the wizard", async () => {
  const { flows, user } = probe();
  act(() => flows().doctor('nowhere'));
  const doctor = await screen.findByRole('dialog', { name: 'Doctor · nowhere' });
  await user.click(await within(doctor).findByRole('button', { name: 'Add a target' }));
  expect(screen.queryByRole('dialog', { name: 'Doctor · nowhere' })).toBeNull();
  expect(hostOf(await screen.findByRole('dialog', { name: 'Add target' }))).toBe('app');
});

/** The doctor on the IPC harness: D.3d's mock engine's doctor has no SSH key fix to offer. */
function doctorOnHarness() {
  clearMocks();
  const h = installHarness();
  h.read('op_start_doctor', [completed(doctorReport('prod-eu'))]);
  h.answer('ssh_key_candidates', []);
  h.answer('target_show', targetReport({ name: 'prod-eu', sshKey: null }));
  return h;
}

test("the doctor's SSH key fix opens the key change above the doctor, which stays", async () => {
  doctorOnHarness();
  const { flows, user } = probe();
  act(() => flows().doctor('prod-eu'));
  const doctor = await screen.findByRole('dialog', { name: 'Doctor · prod-eu' });
  await user.click(await within(doctor).findByRole('button', { name: 'Change SSH key' }));
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  expect(hostOf(form)).toBe('app');
  // Beside the doctor, never inside its panel; the doctor goes inert under it.
  expect(doctor.contains(form)).toBe(false);
  expect(doctor.closest('[inert]') === null).toBe(false);
  await user.click(within(form).getByRole('button', { name: 'Cancel' }));
  expect(screen.getByRole('dialog', { name: 'Doctor · prod-eu' })).toBeDefined();
});

test('a refusal the key change cannot show in a form is shown in the doctor', async () => {
  const h = doctorOnHarness();
  h.answer('target_show', () =>
    Promise.reject(uiError('apprafter::target::not_found', 'target `prod-eu` was not found')),
  );
  const { flows, user } = probe();
  act(() => flows().doctor('prod-eu'));
  const doctor = await screen.findByRole('dialog', { name: 'Doctor · prod-eu' });
  await user.click(await within(doctor).findByRole('button', { name: 'Change SSH key' }));
  expect(await within(doctor).findByText('target `prod-eu` was not found')).toBeDefined();
  expect(screen.queryByRole('dialog', { name: 'Change SSH key' })).toBeNull();
});
