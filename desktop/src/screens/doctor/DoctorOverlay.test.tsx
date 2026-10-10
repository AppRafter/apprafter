// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import { resetOperations } from '../../ipc/operations';
import { ViewFrame } from '../../shell/ViewFrame';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo } from '../../test/fixtures';
import { check, doctorReport } from '../../test/flows';
import {
  cancelled,
  completed,
  failed,
  type Harness,
  installHarness,
  stage,
  uiError,
} from '../../test/ipc';
import { DoctorOverlay } from './DoctorOverlay';

let h: Harness;
beforeEach(() => {
  h = installHarness();
});
afterEach(() => {
  cleanup();
  resetOperations();
  clearMocks();
});

function renderDoctor(target: string, strict = false) {
  const onClose = mock();
  const onAddTarget = mock();
  const onToolchain = mock();
  const onChangeSshKey = mock();
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <ToastProvider>
          <ViewFrame>
            <DoctorOverlay
              target={target}
              onClose={onClose}
              onAddTarget={onAddTarget}
              onToolchain={onToolchain}
              onChangeSshKey={onChangeSshKey}
            />
          </ViewFrame>
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
    { reactStrictMode: strict },
  );
  return { user: userEvent.setup(), onClose, onAddTarget, onToolchain, onChangeSshKey };
}

const runAgain = () => screen.getByRole('button', { name: 'Run again' }) as HTMLButtonElement;

describe('DoctorOverlay', () => {
  test('runs at once for its target, shows the stage while it runs, Cancel cancels', async () => {
    h.read('op_start_doctor', [stage(2, 3, 'Cluster')]); // keeps running
    const { user } = renderDoctor('prod-eu');
    expect(screen.getByRole('dialog', { name: 'Doctor · prod-eu' })).toBeDefined();
    expect(await screen.findByText('Cluster · 2 of 3')).toBeDefined();
    expect(h.of('op_start_doctor')[0]?.args).toEqual({ target: 'prod-eu' });
    expect(runAgain().disabled).toBe(true);
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(h.of('op_cancel').map((c) => c.args)).toEqual([
      { opId: h.started('op_start_doctor')[0] },
    ]);
  });

  test('the report: summary chips, the groups in order, rows, footer', async () => {
    h.read('op_start_doctor', [completed(doctorReport())]);
    renderDoctor('prod-eu');
    expect(await screen.findByText('3 pass')).toBeDefined();
    for (const chip of ['2 warn', '1 fail', '1 skipped']) {
      expect(screen.getByText(chip)).toBeDefined();
    }
    expect(screen.getAllByRole('heading', { level: 3 }).map((e) => e.textContent)).toEqual([
      'Target',
      'Cluster',
      'This computer',
    ]);
    // A skipped check counts in no total (R8): 3 + 2 + 1 ran.
    expect(screen.getByText(/^6 checks · \d\d:\d\d$/)).toBeDefined();
  });

  test('a count of zero has no chip', async () => {
    h.read('op_start_doctor', [
      completed({ target: 'prod-eu', groups: [{ id: 'target', checks: [check()] }] }),
    ]);
    renderDoctor('prod-eu');
    expect(await screen.findByText('1 pass')).toBeDefined();
    expect(screen.queryByText(/\d+ (warn|fail|skipped)/)).toBeNull();
    expect(screen.getByText(/^1 check · /)).toBeDefined();
  });

  test('Run again starts a new run', async () => {
    h.read('op_start_doctor', [completed(doctorReport())]);
    h.read('op_start_doctor', [completed(doctorReport())]);
    const { user } = renderDoctor('prod-eu');
    await screen.findByText('3 pass');
    await user.click(runAgain());
    await waitFor(() => expect(h.of('op_start_doctor')).toHaveLength(2));
    expect(await screen.findByText('3 pass')).toBeDefined();
  });

  test("a missing tool's fix opens the toolchain", async () => {
    h.read('op_start_doctor', [completed(doctorReport())]);
    const { user, onToolchain, onClose } = renderDoctor('prod-eu');
    await user.click(await screen.findByRole('button', { name: 'Show the toolchain' }));
    expect(onToolchain).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();
  });

  test('a target with no SSH key: its fix opens the key change over the doctor, which stays', async () => {
    // test/flows' doctorReport(): the ssh_key row's fix is configure_ssh_key.
    h.read('op_start_doctor', [completed(doctorReport())]);
    const { user, onChangeSshKey, onClose } = renderDoctor('prod-eu');
    await user.click(await screen.findByRole('button', { name: 'Change SSH key' }));
    expect(onChangeSshKey).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();
  });

  test('a stored key path with no file: its fix opens the key change too', async () => {
    h.read('op_start_doctor', [
      completed({
        target: 'prod-eu',
        groups: [
          {
            id: 'target',
            checks: [
              check({
                id: 'ssh_key',
                status: 'fail',
                title: 'SSH key file exists',
                fix: { kind: 'ssh_key_missing', path: '/home/alex/.ssh/gone.pub' },
              }),
            ],
          },
        ],
      }),
    ]);
    const { user, onChangeSshKey, onClose } = renderDoctor('prod-eu');
    await user.click(await screen.findByRole('button', { name: 'Change SSH key' }));
    expect(onChangeSshKey).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();
  });

  test("a missing target's fix closes the doctor, then opens the wizard", async () => {
    h.read('op_start_doctor', [
      completed({
        target: 'gone',
        groups: [
          {
            id: 'target',
            checks: [
              check({
                id: 'target_exists',
                status: 'fail',
                title: 'Target exists',
                fix: { kind: 'add_target', name: 'gone', available: [] },
              }),
            ],
          },
        ],
      }),
    ]);
    const { user, onClose, onAddTarget } = renderDoctor('gone');
    await user.click(await screen.findByRole('button', { name: 'Add a target' }));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(onAddTarget).toHaveBeenCalledTimes(1);
    expect(onClose.mock.invocationCallOrder[0]).toBeLessThan(
      onAddTarget.mock.invocationCallOrder[0] ?? 0,
    );
  });

  test('a failed run shows its error and Run again', async () => {
    h.read('op_start_doctor', [
      failed(uiError('apprafter::desktop::internal', 'the doctor broke')),
    ]);
    renderDoctor('prod-eu');
    expect(await screen.findByText('the doctor broke')).toBeDefined();
    expect(runAgain().disabled).toBe(false);
  });

  test('a cancelled run says so and offers Run again', async () => {
    h.read('op_start_doctor', [cancelled()]);
    renderDoctor('prod-eu');
    expect(await screen.findByText('Doctor was cancelled.')).toBeDefined();
    expect(runAgain().disabled).toBe(false);
    expect(screen.queryByText(/\d+ pass/)).toBeNull();
  });

  test('under StrictMode the double mount still ends on a report', async () => {
    // The first mount's run is cancelled by the simulated unmount and never answers.
    let started = 0;
    h.answer('op_start_doctor', () => {
      started += 1;
      return started === 1 ? 81 : 82;
    });
    h.operation(81, []);
    h.operation(82, [completed(doctorReport())]);
    renderDoctor('prod-eu', true);
    expect(await screen.findByText('3 pass')).toBeDefined();
    expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 81 }]);
  });

  test('Cancel before Rust answered cancels the run once it has an id', async () => {
    let answer = (_opId: number) => {};
    h.answer(
      'op_start_doctor',
      () =>
        new Promise<number>((resolve) => {
          answer = resolve;
        }),
    );
    h.operation(90, []);
    const { user } = renderDoctor('prod-eu');
    await user.click(await screen.findByRole('button', { name: 'Cancel' }));
    expect(h.of('op_cancel')).toHaveLength(0);
    answer(90);
    await waitFor(() => expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 90 }]));
  });
});
