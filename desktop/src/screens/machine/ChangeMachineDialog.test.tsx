// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, spyOn, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import { endedAwaySnapshot, resetEndedAway } from '../../ipc/away';
import { resetOperations } from '../../ipc/operations';
import { ViewFrame } from '../../shell/ViewFrame';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo } from '../../test/fixtures';
import { catalogue, machineSet, planParts } from '../../test/flows';
import {
  cancelled,
  completed,
  failed,
  type Harness,
  installHarness,
  uiError,
} from '../../test/ipc';
import { ChangeMachineDialog, type MachineNow } from './ChangeMachineDialog';

let h: Harness;
beforeEach(() => {
  h = installHarness();
  h.read('op_start_machine_catalogue', [completed(catalogue())]);
  h.read('op_start_region_latencies', [completed([{ region: 'nbg1', latencyMs: 38 }])]);
});
afterEach(() => {
  cleanup();
  resetOperations();
  resetEndedAway();
  clearMocks();
});

function renderChange(target: string, now: MachineNow, strict = false) {
  const onClose = mock();
  const client = createQueryClient();
  const invalidated = spyOn(client, 'invalidateQueries');
  const view = render(
    <QueryClientProvider client={client}>
      <PlatformContext value={appInfo()}>
        <ToastProvider>
          <ViewFrame>
            <ChangeMachineDialog target={target} now={now} onClose={onClose} />
          </ViewFrame>
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
    { reactStrictMode: strict },
  );
  return { user: userEvent.setup(), onClose, invalidated, unmount: view.unmount };
}
const radio = (name: string) => screen.getByRole('radio', { name }) as HTMLInputElement;
const apply = () => screen.getByRole('button', { name: 'Apply machine' }) as HTMLButtonElement;
const STAGING: MachineNow = { region: 'nbg1', serverType: 'cx22' };

test("opens on the target's machine: its catalogue, no stepper, nothing to apply yet", async () => {
  renderChange('staging', STAGING);
  expect(screen.getByRole('dialog', { name: 'Change machine · staging' })).toBeDefined();
  expect(document.querySelector('[aria-current="step"]')).toBeNull();
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  expect(h.of('op_start_machine_catalogue')[0]?.args).toEqual({
    source: { kind: 'target', name: 'staging' },
  });
  expect(apply().disabled).toBe(true);
});

test('opens on the machine the target is set to, not the recommended one', async () => {
  renderChange('staging', { region: 'nbg1', serverType: 'cpx22' });
  await waitFor(() => expect(radio('cpx22').checked).toBe(true));
  expect(radio('cx22').checked).toBe(false);
  expect(apply().disabled).toBe(true);
});

test("opens in the target's region", async () => {
  renderChange('staging', { region: 'hel1', serverType: 'cx22' });
  expect(await screen.findByRole('table', { name: 'Machines in hel1' })).toBeDefined();
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  expect(apply().disabled).toBe(true);
});

test('a type sold out in its region opens on the recommended one, which can be applied', async () => {
  // test/flows' catalogue(): cx32 is offered in nbg1 but not available.
  renderChange('staging', { region: 'nbg1', serverType: 'cx32' });
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  expect(apply().disabled).toBe(false);
});

test('under StrictMode the double mount still ends on the catalogue', async () => {
  // The first mount's read is cancelled by the simulated unmount and never answers; only the
  // read the second mount starts can bring the catalogue.
  let started = 0;
  h.answer('op_start_machine_catalogue', () => {
    started += 1;
    return started === 1 ? 81 : 82;
  });
  h.operation(81, []);
  h.operation(82, [completed(catalogue())]);
  renderChange('staging', STAGING, true);
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 81 }]);
});

test('Apply plans, runs the Bounded plan at once, refreshes the target and closes', async () => {
  h.plan(
    'op_plan_target_machine',
    planParts({ class: 'bounded', title: 'Set the machine of staging', target: 'staging' }),
    [completed(machineSet())],
  );
  const { user, onClose, invalidated } = renderChange('staging', STAGING);
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  await user.click(radio('cpx22'));
  expect(apply().disabled).toBe(false);
  await user.click(apply());
  expect(await screen.findByText('Machine for “staging”: cpx22 in nbg1.')).toBeDefined();
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(h.of('op_plan_target_machine')[0]?.args).toEqual({
    name: 'staging',
    sku: 'cpx22',
    region: 'nbg1',
  });
  expect(h.of('op_execute')).toHaveLength(1);
  // D.3d's keys (state/targets.ts), pinned here by value.
  expect(invalidated.mock.calls.map(([filters]) => filters?.queryKey)).toEqual([
    ['targets'],
    ['target', 'staging'],
  ]);
});

test("a provisioned target's refusal is shown and nothing runs", async () => {
  h.answer('op_plan_target_machine', () =>
    Promise.reject(
      uiError(
        'apprafter::target::provisioned',
        'target `staging` has a provisioned server, so its machine or region cannot change',
      ),
    ),
  );
  const { user } = renderChange('staging', STAGING);
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  await user.click(radio('cpx22'));
  await user.click(apply());
  expect(
    await screen.findByText(
      'target `staging` has a provisioned server, so its machine or region cannot change',
    ),
  ).toBeDefined();
  expect(h.of('op_execute')).toHaveLength(0);
  expect(apply().disabled).toBe(false);
});

test('a run that fails says why and the dialog stays open', async () => {
  h.plan('op_plan_target_machine', planParts({ target: 'staging' }), [
    failed(uiError('apprafter::provider::request_failed', 'the Hetzner Cloud API did not answer')),
  ]);
  const { user, onClose } = renderChange('staging', STAGING);
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  await user.click(radio('cpx22'));
  await user.click(apply());
  expect(await screen.findByText('the Hetzner Cloud API did not answer')).toBeDefined();
  expect(onClose).not.toHaveBeenCalled();
});

test("no region and no type yet: the CLI's default region and its recommended offer, Apply enabled", async () => {
  renderChange('staging', { region: null, serverType: null });
  expect(await screen.findByRole('table', { name: 'Machines in nbg1' })).toBeDefined();
  await waitFor(() => expect(radio('cx22').checked).toBe(true)); // cx22 is nbg1's recommended offer
  expect(apply().disabled).toBe(false);
});

test('a catalogue that cannot be read says why, and Try again reads again', async () => {
  h.answer('op_start_machine_catalogue', () =>
    Promise.reject(uiError('apprafter::target::not_found', 'target `staging` was not found')),
  );
  const { user } = renderChange('staging', STAGING);
  expect(await screen.findByText('target `staging` was not found')).toBeDefined();
  expect(apply().disabled).toBe(true);
  h.answer('op_start_machine_catalogue', 77);
  h.operation(77, [completed(catalogue())]);
  await user.click(screen.getByRole('button', { name: 'Try again' }));
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
});

test('a catalogue read cancelled elsewhere says so, with Try again', async () => {
  h.answer('op_start_machine_catalogue', 78);
  h.operation(78, [cancelled()]);
  renderChange('staging', STAGING);
  expect(await screen.findByText('Reading the catalogue was cancelled.')).toBeDefined();
  expect(screen.getByRole('button', { name: 'Try again' })).toBeDefined();
});

test('a latency read that fails says why, with Try again', async () => {
  h.answer('op_start_region_latencies', () =>
    Promise.reject(uiError('apprafter::desktop::internal', 'the probe broke')),
  );
  const { user } = renderChange('staging', STAGING);
  expect(await screen.findByText('Latency could not be measured: the probe broke')).toBeDefined();
  h.answer('op_start_region_latencies', 79);
  h.operation(79, [completed([{ region: 'nbg1', latencyMs: 38 }])]);
  await user.click(screen.getByRole('button', { name: 'Try again' }));
  expect(await screen.findByText('38 ms')).toBeDefined();
});

test('a change that ends after the dialog went keeps its end for the app', async () => {
  let channel: { id: number } | null = null;
  h.plan('op_plan_target_machine', planParts({ title: 'Set the machine of staging' }), []);
  h.answer('op_execute', ({ onEvent }: Record<string, unknown>) => {
    channel = onEvent as { id: number };
    return 2;
  });
  const { user, unmount } = renderChange('staging', STAGING);
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  await user.click(radio('cpx22'));
  await user.click(apply());
  await waitFor(() => expect(channel).not.toBeNull());
  unmount();
  const internals = (
    window as unknown as { __TAURI_INTERNALS__: { runCallback(id: number, data: unknown): void } }
  ).__TAURI_INTERNALS__;
  act(() => {
    internals.runCallback(channel?.id ?? -1, { index: 0, message: completed(machineSet()) });
  });
  await waitFor(() =>
    expect(endedAwaySnapshot()).toEqual([
      {
        opId: h.started('op_plan_target_machine')[0] ?? -1,
        text: 'Set the machine of staging: done.',
        failed: false,
      },
    ]),
  );
});

test("a Destructive plan opens D.3d's PlanConfirm first (guard)", async () => {
  h.plan(
    'op_plan_target_machine',
    planParts({ class: 'destructive', title: 'Replace the machine of staging', target: 'staging' }),
    [completed(machineSet())],
  );
  const { user } = renderChange('staging', STAGING);
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  await user.click(radio('cpx22'));
  await user.click(apply());
  expect(
    await screen.findByRole('dialog', { name: 'Replace the machine of staging' }),
  ).toBeDefined();
  expect(h.of('op_execute')).toHaveLength(0);
});

test("confirmed in its dialog, a Destructive plan runs once; the dialog's submit is not the frame's Apply", async () => {
  h.plan(
    'op_plan_target_machine',
    planParts({ class: 'destructive', title: 'Replace the machine of staging', target: 'staging' }),
    [completed(machineSet())],
  );
  const { user, onClose } = renderChange('staging', STAGING);
  await waitFor(() => expect(radio('cx22').checked).toBe(true));
  await user.click(radio('cpx22'));
  await user.click(apply());
  const dialog = await screen.findByRole('dialog', { name: 'Replace the machine of staging' });
  await user.click(within(dialog).getByRole('button', { name: 'Apply machine' }));
  expect(await screen.findByText('Machine for “staging”: cpx22 in nbg1.')).toBeDefined();
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(h.of('op_plan_target_machine')).toHaveLength(1);
  expect(h.of('op_execute')).toHaveLength(1);
});
