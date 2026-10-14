// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A screen as the shell hosts it: a fresh query client, app_info, the toasts (with their
// viewport), a view frame for its overlays, and the app's own overlays beside the view.
import { QueryClientProvider } from '@tanstack/react-query';
import { render } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { ToastProvider, ToastViewport } from '../components/Toast';
import type { AppInfo } from '../ipc/generated/AppInfo';
import { AppOverlayContext, useAppOverlayHost } from '../shell/AppOverlays';
import { ViewFrame } from '../shell/ViewFrame';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo } from './fixtures';

/** A screen's host as the shell has it: a view frame, and the app's own overlays beside it. */
export function ScreenHost({ children }: { children: ReactNode }) {
  const app = useAppOverlayHost();
  return (
    <AppOverlayContext value={app.show}>
      <ViewFrame>{children}</ViewFrame>
      {app.overlays}
    </AppOverlayContext>
  );
}

export function renderScreen(ui: ReactNode, { info = appInfo() }: { info?: AppInfo } = {}) {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={info}>
        <ToastProvider>
          <ScreenHost>{ui}</ScreenHost>
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}
