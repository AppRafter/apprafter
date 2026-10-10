// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { act, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { ToastProvider } from '../components/Toast';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { Settings } from '../ipc/generated/Settings';
import { settings } from '../test/fixtures';
import { settleIpc } from '../test/settle';
import { createQueryClient } from './queryClient';
import { SETTINGS_KEY, useSaveSettings, useSettings } from './settings';

let stored: Settings;
let readFails: boolean;

const uiError = (code: string, message: string) => ({
  code,
  message,
  help: null,
  causes: [],
  fields: {},
});

beforeEach(() => {
  stored = settings({ theme: 'dark' });
  readFails = false;
  mockIPC((cmd) => {
    if (cmd === 'settings_get') {
      return readFails
        ? Promise.reject(uiError(DESKTOP_ERROR_CODES.SETTINGS_IO, 'settings.json is unreadable'))
        : stored;
    }
    if (cmd === 'settings_set') {
      return Promise.reject(uiError(DESKTOP_ERROR_CODES.SETTINGS_IO, 'disk full'));
    }
    return null;
  });
});

afterEach(async () => {
  await settleIpc();
  clearMocks();
});

test('a refused save whose read-back fails leaves what Rust confirmed, not the refused choice', async () => {
  const client = createQueryClient();
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>
      <ToastProvider>{children}</ToastProvider>
    </QueryClientProvider>
  );
  const { result } = renderHook(() => ({ read: useSettings(), save: useSaveSettings() }), {
    wrapper,
  });
  await waitFor(() => expect(result.current.read.data?.theme).toBe('dark'));
  readFails = true;
  act(() => result.current.save({ theme: 'light' }));
  // Optimistic meanwhile: the theme the owner picked shows at once.
  expect(client.getQueryData<Settings>(SETTINGS_KEY)?.theme).toBe('light');
  await waitFor(() => expect(result.current.read.isError).toBe(true));
  // The read-back failed, and the refused theme is not what stays applied.
  expect(client.getQueryData<Settings>(SETTINGS_KEY)?.theme).toBe('dark');
});
