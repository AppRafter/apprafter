// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import type { OpEvent } from '../../ipc/generated/OpEvent';
import { resetOperations } from '../../ipc/operations';
import { ViewFrame } from '../../shell/ViewFrame';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo } from '../../test/fixtures';
import { catalogue, planParts, targetAdded } from '../../test/flows';
import {
  cancelled,
  completed,
  failed,
  type Harness,
  installHarness,
  uiError,
} from '../../test/ipc';
import { AddTargetWizard, DRAFT_GONE } from './AddTargetWizard';

const TOKEN = 'A1'.repeat(32);
let h: Harness;
beforeEach(() => {
  h = installHarness();
  h.answer('target_list', { targets: [], unreadable: [], cliDefault: { status: 'unset' } });
});
afterEach(() => {
  cleanup();
  resetOperations();
  clearMocks();
});

const currentStep = () => document.querySelector('[aria-current="step"]')?.textContent ?? '';

function renderWizard(os: 'linux' | 'windows' = 'linux') {
  const onClose = mock();
  const { unmount } = render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo({ os })}>
        <ToastProvider>
          <ViewFrame>
            <AddTargetWizard onClose={onClose} />
          </ViewFrame>
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return { user: userEvent.setup(), onClose, unmount };
}

describe('provider and token', () => {
  test('Hetzner Cloud is the one provider; Continue waits for a well-formed token', async () => {
    const { user } = renderWizard();
    expect((screen.getByRole('radio', { name: /Hetzner Cloud/ }) as HTMLInputElement).checked).toBe(
      true,
    );
    expect(screen.queryByRole('radio', { name: /AWS|Managed/ })).toBeNull();
    const next = screen.getByRole('button', { name: 'Verify and continue' }) as HTMLButtonElement;
    expect(next.disabled).toBe(true);
    await user.type(screen.getByLabelText('API token'), 'abc');
    expect(screen.getByText('3/64 characters')).toBeDefined();
    expect(next.disabled).toBe(true);
  });

  test('the secret copy is the OS-true one', () => {
    renderWizard('windows');
    expect(screen.getByText('Saved in a file in your user profile')).toBeDefined();
  });

  test('the token crosses IPC once, the field is emptied, and Machine is the next step', async () => {
    h.read('op_start_verify_token', [completed({ draftId: 7, elapsedMs: 182 })]);
    h.read('op_start_machine_catalogue', [completed(catalogue())]);
    h.read('op_start_region_latencies', [completed([{ region: 'nbg1', latencyMs: 38 }])]);
    const { user } = renderWizard();
    await user.type(screen.getByLabelText('API token'), TOKEN);
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    await waitFor(() => expect(currentStep()).toContain('Machine'));
    expect(await screen.findByRole('table', { name: 'Machines in nbg1' })).toBeDefined();
    // The catalogue names the draft, never the token.
    const carrying = h.calls
      .filter((c) => JSON.stringify(c.args).includes(TOKEN))
      .map((c) => c.cmd);
    expect(carrying).toEqual(['op_start_verify_token']);
    expect(h.of('op_start_verify_token')[0]?.args).toEqual({
      provider: 'hetzner-cloud',
      token: TOKEN,
    });
    expect(h.of('op_start_machine_catalogue')[0]?.args).toEqual({
      source: { kind: 'draft', draftId: 7 },
    });
    expect(document.body.innerHTML).not.toContain(TOKEN);
  });

  test('a rejected token stays on the step with the core message', async () => {
    h.read('op_start_verify_token', [
      failed(
        uiError('apprafter::target::token_rejected', 'Hetzner Cloud rejected the token (HTTP 401)'),
      ),
    ]);
    const { user } = renderWizard();
    await user.type(screen.getByLabelText('API token'), TOKEN);
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    expect(await screen.findByText('Hetzner Cloud rejected the token (HTTP 401)')).toBeDefined();
    expect(screen.getByLabelText('API token')).toBeDefined();
  });

  test('back on the provider step after a verify: "Token verified", and another token discards the draft', async () => {
    h.read('op_start_verify_token', [completed({ draftId: 7, elapsedMs: 182 })]);
    const { user } = renderWizard();
    await user.type(screen.getByLabelText('API token'), TOKEN);
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    await waitFor(() => expect(currentStep()).toContain('Machine'));
    await user.click(screen.getByRole('button', { name: 'Back' }));
    expect(screen.getByText('Token verified')).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Use another token' }));
    expect(h.of('target_draft_discard').map((c) => c.args)).toEqual([{ draftId: 7 }]);
    expect(screen.getByLabelText('API token')).toBeDefined();
  });

  test('while verifying the frame is busy: Close and Next wait', async () => {
    h.read('op_start_verify_token', []); // keeps running
    const { user } = renderWizard();
    await user.type(screen.getByLabelText('API token'), TOKEN);
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    expect(await screen.findByRole('button', { name: 'Verifying…' })).toBeDefined();
    expect((screen.getByRole('button', { name: 'Close' }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  test('the wizard going away discards the draft it holds', async () => {
    h.read('op_start_verify_token', [completed({ draftId: 7, elapsedMs: 182 })]);
    const { user, unmount } = renderWizard();
    await user.type(screen.getByLabelText('API token'), TOKEN);
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    await waitFor(() => expect(currentStep()).toContain('Machine'));
    unmount();
    expect(h.of('target_draft_discard').map((c) => c.args)).toEqual([{ draftId: 7 }]);
  });
});

/**
 * Verify (draft 7), then the machine step; `catalogueEvents` is how the catalogue read goes. It
 * waits for the catalogue read to start, not for the step's name: a lost draft leaves the step
 * again at once.
 */
async function toMachine(
  user: ReturnType<typeof userEvent.setup>,
  catalogueEvents: readonly OpEvent[] = [completed(catalogue())],
) {
  h.read('op_start_verify_token', [completed({ draftId: 7, elapsedMs: 182 })]);
  h.read('op_start_machine_catalogue', catalogueEvents);
  h.read('op_start_region_latencies', [completed([{ region: 'nbg1', latencyMs: 38 }])]);
  await user.type(screen.getByLabelText('API token'), TOKEN);
  await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
  await waitFor(() => expect(h.of('op_start_machine_catalogue')).toHaveLength(1));
}

describe('after the wizard went', () => {
  test('a verify that completes after the wizard is gone discards its draft', async () => {
    let answer = (_opId: number) => {};
    h.answer(
      'op_start_verify_token',
      () =>
        new Promise<number>((resolve) => {
          answer = resolve;
        }),
    );
    h.operation(77, [completed({ draftId: 9, elapsedMs: 182 })]);
    const { user, unmount } = renderWizard();
    await user.type(screen.getByLabelText('API token'), TOKEN);
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    unmount();
    answer(77);
    await waitFor(() =>
      expect(h.of('target_draft_discard').map((c) => c.args)).toEqual([{ draftId: 9 }]),
    );
    expect(h.of('op_cancel').map((c) => c.args)).toEqual([{ opId: 77 }]);
  });

  test('a catalogue that lands after the wizard is gone starts no latency read', async () => {
    let answer = (_opId: number) => {};
    const { user, unmount } = renderWizard();
    h.answer(
      'op_start_machine_catalogue',
      () =>
        new Promise<number>((resolve) => {
          answer = resolve;
        }),
    );
    h.operation(78, [completed(catalogue())]);
    await toMachine(user, []);
    unmount();
    answer(78);
    await waitFor(() => expect(h.of('op_discard').map((c) => c.args)).toContainEqual({ opId: 78 }));
    expect(h.of('op_start_region_latencies')).toHaveLength(0);
  });
});

describe('machine step', () => {
  test('the catalogue is read once per draft; the latencies after it, with its region codes', async () => {
    const { user } = renderWizard();
    await toMachine(user);
    await screen.findByRole('table', { name: 'Machines in nbg1' });
    await waitFor(() => expect(h.of('op_start_region_latencies')).toHaveLength(1));
    // The catalogue's regions in its own order (the core's, by code); the probes run at once.
    expect(h.of('op_start_region_latencies')[0]?.args).toEqual({
      regions: ['fsn1', 'hel1', 'nbg1', 'sin'],
    });
    await user.click(screen.getByRole('button', { name: 'Back' }));
    await user.click(screen.getByRole('button', { name: 'Continue' })); // step 0 with a draft
    await waitFor(() => expect(currentStep()).toContain('Machine'));
    expect(h.of('op_start_machine_catalogue')).toHaveLength(1);
  });

  test('a lost draft ends the read: back to the token, the field empty, the note shown', async () => {
    const { user } = renderWizard();
    await toMachine(user, [failed(uiError(DESKTOP_ERROR_CODES.DRAFT_EXPIRED))]);
    expect(await screen.findByText(DRAFT_GONE)).toBeDefined();
    expect(currentStep()).toContain('Provider');
    expect((screen.getByLabelText('API token') as HTMLInputElement).value).toBe('');
  });

  test('a draft Rust refuses at the command (no read starts) goes back to the token the same way', async () => {
    h.answer('op_start_machine_catalogue', () =>
      Promise.reject(uiError(DESKTOP_ERROR_CODES.DRAFT_EXPIRED)),
    );
    const { user } = renderWizard();
    await toMachine(user, []); // the queued read is never used: the answer wins
    expect(await screen.findByText(DRAFT_GONE)).toBeDefined();
    expect((screen.getByLabelText('API token') as HTMLInputElement).value).toBe('');
  });

  test('another catalogue failure shows its message, and Try again reads again', async () => {
    const { user } = renderWizard();
    await toMachine(user, [
      failed(
        uiError('apprafter::provider::request_failed', 'the Hetzner Cloud API did not answer'),
      ),
    ]);
    expect(await screen.findByText('the Hetzner Cloud API did not answer')).toBeDefined();
    h.read('op_start_machine_catalogue', [completed(catalogue())]);
    await user.click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByRole('table', { name: 'Machines in nbg1' })).toBeDefined();
    expect(h.of('op_start_machine_catalogue')).toHaveLength(2);
  });

  test('a catalogue read cancelled elsewhere says so, and Try again reads again', async () => {
    const { user } = renderWizard();
    await toMachine(user, [cancelled()]);
    expect(await screen.findByText('Reading the catalogue was cancelled.')).toBeDefined();
    h.read('op_start_machine_catalogue', [completed(catalogue())]);
    await user.click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByRole('table', { name: 'Machines in nbg1' })).toBeDefined();
  });

  test('while the catalogue is read the step says so and Continue waits', async () => {
    const { user } = renderWizard();
    await toMachine(user, []); // keeps running
    expect(await screen.findByText("Reading the provider's catalogue…")).toBeDefined();
    expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  test('Continue needs a choosable machine: fsn1 has no recommended offer, so a row must be chosen', async () => {
    const { user } = renderWizard();
    await toMachine(user);
    await screen.findByRole('table', { name: 'Machines in nbg1' });
    expect(screen.getByText('Prices from the provider, excl. VAT')).toBeDefined();
    await user.click(screen.getByText('Falkenstein'));
    await screen.findByRole('table', { name: 'Machines in fsn1' });
    const next = screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement;
    expect(next.disabled).toBe(true);
    // test/flows' catalogue(): fsn1 offers cpx22 only.
    await user.click(screen.getByRole('radio', { name: 'cpx22' }));
    expect(next.disabled).toBe(false);
  });
});

/** Tasks 13–14's path to the details step: verify, the catalogue (nbg1, cx22 recommended), Continue. */
async function toDetails(user: ReturnType<typeof userEvent.setup>) {
  h.answer('ssh_key_candidates', [
    {
      path: '/home/alex/.ssh/id_ed25519.pub',
      display: '~/.ssh/id_ed25519.pub',
      algo: 'ssh-ed25519',
      comment: 'alex@host',
    },
  ]);
  await toMachine(user);
  await screen.findByRole('table', { name: 'Machines in nbg1' });
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await waitFor(() => expect(currentStep()).toContain('Details'));
}

const saveButton = () => screen.getByRole('button', { name: 'Save target' }) as HTMLButtonElement;

describe('details and save', () => {
  test('a Bounded plan is saved by the Save click: no dialog; toast, list refreshed, closed', async () => {
    h.plan('op_plan_target_add', planParts({ class: 'bounded', title: 'Add target lab-2' }), [
      completed(targetAdded({ name: 'lab-2', cliDefault: { from: null, to: 'lab-2' } })),
    ]);
    const { user, onClose } = renderWizard();
    await toDetails(user);
    await user.type(screen.getByLabelText('Target name'), 'lab-2');
    await user.click(screen.getByRole('radio', { name: /Team \(T2\)/ }));
    await user.click(saveButton());
    expect(
      await screen.findByText('Target “lab-2” saved. Your CLI default is now lab-2.'),
    ).toBeDefined();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(h.of('op_plan_target_add')[0]?.args).toEqual({
      args: {
        name: 'lab-2',
        provider: 'hetzner-cloud',
        draftId: 7,
        sshKey: '/home/alex/.ssh/id_ed25519.pub',
        region: 'nbg1',
        tier: 'team',
        serverType: 'cx22',
      },
    });
    expect(h.of('op_execute')).toHaveLength(1);
    expect(screen.queryByRole('dialog', { name: /confirm/i })).toBeNull();
    await waitFor(() => expect(h.of('target_list').length).toBeGreaterThan(1)); // invalidated: read again
    expect(h.of('target_draft_discard')).toHaveLength(0); // the plan took the draft
  });

  test('a Destructive plan is never run without its dialog (guard)', async () => {
    h.plan(
      'op_plan_target_add',
      planParts({ class: 'destructive', title: 'Replace target lab-2' }),
      [completed(targetAdded({ name: 'lab-2' }))],
    );
    const { user } = renderWizard();
    await toDetails(user);
    await user.type(screen.getByLabelText('Target name'), 'lab-2');
    await user.click(saveButton());
    expect(await screen.findByRole('dialog', { name: 'Replace target lab-2' })).toBeDefined();
    expect(h.of('op_execute')).toHaveLength(0);
  });

  test("confirmed in its dialog, a Destructive plan runs once and saves; the wizard's own Save does not fire", async () => {
    h.plan(
      'op_plan_target_add',
      planParts({ class: 'destructive', title: 'Replace target lab-2' }),
      [completed(targetAdded({ name: 'lab-2' }))],
    );
    const { user, onClose } = renderWizard();
    await toDetails(user);
    await user.type(screen.getByLabelText('Target name'), 'lab-2');
    await user.click(saveButton());
    const dialog = await screen.findByRole('dialog', { name: 'Replace target lab-2' });
    await user.click(within(dialog).getByRole('button', { name: 'Save target' }));
    expect(await screen.findByText('Target “lab-2” saved.')).toBeDefined();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(h.of('op_plan_target_add')).toHaveLength(1);
    expect(h.of('op_execute')).toHaveLength(1);
    expect(screen.queryByText(/Enter it again to retry/)).toBeNull();
  });

  test('a refused plan keeps the draft and says why; Save works again after a fix', async () => {
    h.answer('op_plan_target_add', () =>
      Promise.reject(
        uiError('apprafter::target::exists', 'target `prod-eu` already exists', {
          name: 'prod-eu',
        }),
      ),
    );
    const { user } = renderWizard();
    await toDetails(user);
    await user.type(screen.getByLabelText('Target name'), 'prod-eu');
    await user.click(saveButton());
    expect(await screen.findByText('target `prod-eu` already exists')).toBeDefined();
    expect(saveButton().disabled).toBe(false);
  });

  test('a failed save needs the token again: the button says so and goes there', async () => {
    h.plan('op_plan_target_add', planParts({}), [
      failed(
        uiError('apprafter::target::token_rejected', 'Hetzner Cloud rejected the token (HTTP 401)'),
      ),
    ]);
    const { user } = renderWizard();
    await toDetails(user);
    await user.type(screen.getByLabelText('Target name'), 'lab-2');
    await user.click(saveButton());
    expect(await screen.findByText('Hetzner Cloud rejected the token (HTTP 401)')).toBeDefined();
    await user.click(screen.getByRole('button', { name: 'Enter the token again' }));
    expect(screen.getByLabelText('API token')).toBeDefined();
    expect(screen.queryByLabelText('Target name')).toBeNull();
  });

  test('name rules inline, a taken name inline (target_list), Save disabled meanwhile', async () => {
    h.answer('target_list', {
      targets: [
        {
          name: 'prod-eu',
          provider: 'hetzner-cloud',
          region: 'nbg1',
          serverType: null,
          defaultTier: null,
          tierLevel: null,
          isCliDefault: true,
        },
      ],
      unreadable: [],
      cliDefault: { status: 'set', name: 'prod-eu' },
    });
    const { user } = renderWizard();
    await toDetails(user);
    const field = screen.getByLabelText('Target name');
    await user.type(field, '-x');
    expect(screen.getByText('No dash at the start or the end.')).toBeDefined(); // D.3d's nameMessage('edge_dash')
    await user.clear(field);
    await user.type(field, 'prod-eu');
    expect(await screen.findByText('A target named prod-eu exists.')).toBeDefined();
    expect(saveButton().disabled).toBe(true);
  });

  test('SSH: the first key is chosen; Other path is checked on leaving the field; Skip sends none', async () => {
    h.answer('ssh_key_inspect', ({ path }: Record<string, unknown>) => ({
      path,
      display: String(path),
      exists: false,
      algo: null,
    }));
    h.plan('op_plan_target_add', planParts({}), [completed(targetAdded({ name: 'lab-2' }))]);
    const { user } = renderWizard();
    await toDetails(user);
    await user.type(screen.getByLabelText('Target name'), 'lab-2');
    expect(
      (screen.getByRole('radio', { name: '~/.ssh/id_ed25519.pub' }) as HTMLInputElement).checked,
    ).toBe(true);
    await user.click(screen.getByRole('radio', { name: 'Other path…' }));
    await user.type(screen.getByLabelText('Path to a public key'), '/nowhere/key.pub');
    await user.tab();
    expect(await screen.findByText('No file at that path.')).toBeDefined();
    expect(h.of('ssh_key_inspect')[0]?.args).toEqual({ path: '/nowhere/key.pub' });
    expect(saveButton().disabled).toBe(true);
    await user.click(screen.getByRole('radio', { name: 'Skip' }));
    await user.click(saveButton());
    await screen.findByText('Target “lab-2” saved.');
    const planned = h.of('op_plan_target_add')[0]?.args.args as { sshKey: unknown } | undefined;
    expect(planned?.sshKey).toBeNull();
  });
});
