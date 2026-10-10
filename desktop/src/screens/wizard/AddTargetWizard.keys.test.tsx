// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The details step's SSH key against D.3d's mock engine, which answers as Rust does: `~/`
// expanded to the home, a relative path refused, a private key or a file that is not a key
// named as such, and a `.pub` that holds no key listed but not choosable. The page sends what was
// typed; the expansion and the refusals are Rust's (and the mock's).
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import * as api from '../../ipc/api';
import { resetEndedAway } from '../../ipc/away';
import { installMockIpc } from '../../ipc/mock';
import { MOCK_HOME } from '../../ipc/mock/fixtures';
import { resetOperations } from '../../ipc/operations';
import { ViewFrame } from '../../shell/ViewFrame';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo } from '../../test/fixtures';
import { settleIpc } from '../../test/settle';
import { AddTargetWizard } from './AddTargetWizard';

const TOKEN = 'A1'.repeat(32);

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  resetEndedAway();
  clearMocks();
});

const currentStep = () => document.querySelector('[aria-current="step"]')?.textContent ?? '';
const saveButton = () => screen.getByRole('button', { name: 'Save target' }) as HTMLButtonElement;

/** The wizard on the mock: verify, the catalogue (nbg1, cx22 recommended), then Details. */
async function toDetails() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <ToastProvider>
          <ViewFrame>
            <AddTargetWizard onClose={mock()} />
          </ViewFrame>
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  const user = userEvent.setup();
  await user.type(screen.getByLabelText('API token'), TOKEN);
  await user.click(screen.getByRole('button', { name: 'Verify and continue' }));
  await screen.findByRole('table', { name: 'Machines in nbg1' });
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await waitFor(() => expect(currentStep()).toContain('Details'));
  await user.type(screen.getByLabelText('Target name'), 'lab-2');
  return user;
}

async function typePath(user: ReturnType<typeof userEvent.setup>, path: string) {
  await user.click(screen.getByRole('radio', { name: 'Other path…' }));
  const field = screen.getByLabelText('Path to a public key');
  await user.clear(field);
  await user.type(field, path);
  await user.tab();
}

test('a .pub that holds no key is listed but cannot be chosen; the first public key is', async () => {
  await toDetails();
  const old = (await screen.findByRole('radio', { name: '~/.ssh/old.pub' })) as HTMLInputElement;
  expect(old.disabled).toBe(true);
  expect(screen.getByText('not an SSH public key')).toBeDefined();
  expect(
    (screen.getByRole('radio', { name: '~/.ssh/id_ed25519.pub' }) as HTMLInputElement).checked,
  ).toBe(true);
});

test('a ~/ path is expanded by Rust; the target is saved with the full path', async () => {
  const user = await toDetails();
  await typePath(user, '~/.ssh/work.pub');
  expect(await screen.findByText('Found · ssh-rsa')).toBeDefined();
  await user.click(saveButton());
  await screen.findByText(/Target “lab-2” saved/);
  expect((await api.targetShow('lab-2')).sshKey?.path).toBe(`${MOCK_HOME}/.ssh/work.pub`);
});

test('a private key is named as one, with its public half, and cannot be saved', async () => {
  const user = await toDetails();
  await typePath(user, '~/.ssh/id_ed25519');
  expect(
    await screen.findByText(
      '~/.ssh/id_ed25519 is a private key: choose its public half, the .pub file next to it.',
    ),
  ).toBeDefined();
  expect(saveButton().disabled).toBe(true);
});

test("a relative path is Rust's refusal, said as it is", async () => {
  const user = await toDetails();
  await typePath(user, 'keys/work.pub');
  expect(await screen.findByText('`keys/work.pub` is not a full path')).toBeDefined();
  expect(saveButton().disabled).toBe(true);
});
