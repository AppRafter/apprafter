// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A store locked from a terminal while the app was unlocked, on the mock IPC (controller, d4a-g
// run 5): the app locks, and its lock screen opens it again with the store's secret on every
// route the mock has. From run 3 the OS opened nothing there, while the page offered only the
// system's Unlock on Windows, macOS and Linux with polkit; Settings › Lock's lock screen field
// (StoreSecretForm) is the way (D.4a T75).
import { afterEach, expect, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { App } from '../App';
import * as api from '../ipc/api';
import { resetEndedAway } from '../ipc/away';
import { resetHeldPlans } from '../ipc/heldPlans';
import {
  installMockIpc,
  MOCK_PASSWORD,
  type MockOptions,
  mockLockFromTerminal,
  mockUnlockFromTerminal,
} from '../ipc/mock';
import { MOCK_STORE_PASSWORD } from '../ipc/mock/lock';
import { resetOperations } from '../ipc/operations';
import { STORE_FIELD_PLACEHOLDER } from '../state/lockText';
import { settleIpc } from '../test/settle';

afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  resetHeldPlans();
  resetEndedAway();
  clearMocks();
  delete document.documentElement.dataset.theme;
});

const ROUTES = [
  ['Windows Hello', { os: 'windows' }],
  ['Touch ID or the Mac password', { os: 'macos' }],
  ['polkit', { os: 'linux' }],
  ["Linux's PAM field", { os: 'linux', auth: 'pam' }],
] as const satisfies readonly (readonly [string, MockOptions])[];

test.each(ROUTES)('a store locked from a terminal opens in the app: %s', async (_route, route) => {
  installMockIpc({ opDelayMs: 0, ...route });
  // Unlocked, in files, the route's own way.
  if ('auth' in route) await api.unlockWithPassword(MOCK_PASSWORD);
  else await api.unlock();
  render(<App />);
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();

  await act(async () => {
    await mockLockFromTerminal();
  });
  expect(
    await screen.findByText("Locked: this target store's secrets were locked from a terminal."),
  ).toBeDefined();
  const field = await screen.findByPlaceholderText(STORE_FIELD_PLACEHOLDER);
  // The store's field, not the system's prompt: the OS opens nothing here.
  expect(screen.queryByText('Unlock')).toBeNull();
  await userEvent.setup().type(field, `${MOCK_STORE_PASSWORD}{Enter}`);
  expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
});

// D.4a F17 (D-5's extra): the store locked from a terminal, then put back in files from one while
// the app is still locked. Rust's re-read tells the page; the lock screen stops asking for a lock
// password that no longer exists and offers the route's own way, with no focus and nothing typed.
test.each(ROUTES)(
  "a terminal's lock off under the locked app: the lock screen offers the system's way: %s",
  async (_route, route) => {
    installMockIpc({ opDelayMs: 0, secrets: 'locked', ...route });
    render(<App />);
    expect(await screen.findByPlaceholderText(STORE_FIELD_PLACEHOLDER)).toBeDefined();

    await act(async () => {
      await mockUnlockFromTerminal();
    });
    await waitFor(() =>
      expect(screen.queryByPlaceholderText(STORE_FIELD_PLACEHOLDER) === null).toBe(true),
    );
    expect(screen.getByRole('heading', { name: 'AppRafter is locked' })).toBeDefined();
    const user = userEvent.setup();
    if ('auth' in route) {
      await user.type(screen.getByLabelText('System password'), `${MOCK_PASSWORD}{Enter}`);
    } else {
      await user.click(screen.getByRole('button', { name: 'Unlock' }));
    }
    expect(await screen.findByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    expect(screen.getByText('Target store ~/.config/apprafter · Secrets: in files')).toBeDefined();
  },
);
