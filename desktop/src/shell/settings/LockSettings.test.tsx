// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings › Lock on the commands (WI-476): how the app locks, and nothing of the store's own
// protection, which is the Security page's (screens/security/SecurityPage.test.tsx holds the
// tests that lived here with that code at cb9c74e0).
import { afterEach, describe, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, render, screen, within } from '@testing-library/react';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import * as api from '../../ipc/api';
import { installMockIpc } from '../../ipc/mock';
import { ACTION_LABELS, LIMITS_TITLE } from '../../state/lockText';
import { createQueryClient } from '../../state/queryClient';
import { settleIpc } from '../../test/settle';
import { PlatformGate } from '../PlatformGate';
import { SettingsDialog } from '../SettingsDialog';

afterEach(async () => {
  cleanup();
  await settleIpc();
  clearMocks();
});

/** The arguments of every `cmd` sent from now on (a spy over the mock's invoke). */
function sent(cmd: string): unknown[] {
  const internals = (
    window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown, options?: unknown) => unknown };
    }
  ).__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  const seen: unknown[] = [];
  internals.invoke = (c, args, options) => {
    if (c === cmd) seen.push(args);
    return invoke(c, args, options);
  };
  return seen;
}

const settings = () => screen.getByRole('dialog', { name: 'Settings' });

describe('Settings › Lock on the commands', () => {
  test('no Security eyebrow, nothing of the store, and no lock_info read: the Security page has them (WI-476)', async () => {
    installMockIpc({ opDelayMs: 0 });
    await api.unlock();
    const lockInfos = sent('lock_info');
    render(
      <QueryClientProvider client={createQueryClient()}>
        <PlatformGate>
          <ToastProvider>
            <SettingsDialog onClose={mock()} onSecurity={mock()} />
            <ToastViewport />
          </ToastProvider>
        </PlatformGate>
      </QueryClientProvider>,
    );
    await screen.findByRole('radiogroup', { name: 'Theme' });
    expect(within(settings()).queryByText('Security')).toBeNull();
    for (const label of Object.values(ACTION_LABELS)) {
      expect(within(settings()).queryByRole('button', { name: label })).toBeNull();
    }
    expect(within(settings()).queryByText(LIMITS_TITLE)).toBeNull();
    await settleIpc();
    expect(lockInfos).toEqual([]);
  });
});
