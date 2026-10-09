// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import type { OpEvent } from '../../ipc/generated/OpEvent';
import { resetOperations } from '../../ipc/operations';
import { ViewFrame } from '../../shell/ViewFrame';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo } from '../../test/fixtures';
import { catalogue } from '../../test/flows';
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
