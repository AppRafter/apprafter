// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { QueryClientProvider } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import { ToastProvider } from './components/Toast';
import { windowReady } from './ipc/api';
import { type TargetSummary, TargetsSource } from './screens/targets/targets';
import { LockGate } from './shell/LockGate';
import { PlatformGate } from './shell/PlatformGate';
import { Shell } from './shell/Shell';
import { ThemeController } from './shell/ThemeController';
import { createQueryClient } from './state/queryClient';

/** The window starts hidden (no white flash): show it once the page has painted. */
function useRevealWindow() {
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      windowReady().catch((error: unknown) => console.error('window_ready failed:', error));
    });
    return () => cancelAnimationFrame(frame);
  }, []);
}

export interface AppProps {
  /** The Targets view's list; mock mode passes its fixtures, the real app has none until D.3. */
  targets?: readonly TargetSummary[] | null;
}

export function App({ targets = null }: AppProps) {
  const [client] = useState(createQueryClient);
  useRevealWindow();
  return (
    <QueryClientProvider client={client}>
      <PlatformGate>
        <ThemeController />
        <TargetsSource value={targets}>
          {/* The toasts belong to the shell: a lock takes them away with it. */}
          <LockGate>
            <ToastProvider>
              <Shell />
            </ToastProvider>
          </LockGate>
        </TargetsSource>
      </PlatformGate>
    </QueryClientProvider>
  );
}
