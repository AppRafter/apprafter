// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A screen as the shell hosts it: a fresh query client, app_info, the toasts (with their
// viewport) and a view frame for its overlays.
import { QueryClientProvider } from '@tanstack/react-query';
import { render } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { ToastProvider, ToastViewport } from '../components/Toast';
import type { AppInfo } from '../ipc/generated/AppInfo';
import { ViewFrame } from '../shell/ViewFrame';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo } from './fixtures';

export function renderScreen(ui: ReactNode, { info = appInfo() }: { info?: AppInfo } = {}) {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={info}>
        <ToastProvider>
          <ViewFrame>{ui}</ViewFrame>
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}
