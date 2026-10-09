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
import { completed, failed, type Harness, installHarness, uiError } from '../../test/ipc';
import { AddTargetWizard } from './AddTargetWizard';

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
    const { user } = renderWizard();
    await user.type(screen.getByLabelText('API token'), TOKEN);
    await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
    await waitFor(() => expect(currentStep()).toContain('Machine'));
    const carrying = h.calls
      .filter((c) => JSON.stringify(c.args).includes(TOKEN))
      .map((c) => c.cmd);
    expect(carrying).toEqual(['op_start_verify_token']);
    expect(h.of('op_start_verify_token')[0]?.args).toEqual({
      provider: 'hetzner-cloud',
      token: TOKEN,
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
