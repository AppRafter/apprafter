// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import { ToastProvider } from './components/Toast';
import { type TargetSummary, TargetsSource } from './screens/targets/targets';
import { LockGate } from './shell/LockGate';
import { PlatformGate } from './shell/PlatformGate';
import { RevealWindow } from './shell/reveal';
import { Shell } from './shell/Shell';
import { ThemeController } from './shell/ThemeController';
import { createQueryClient } from './state/queryClient';

export interface AppProps {
  /** The Targets view's list; mock mode passes its fixtures, the real app has none until D.3. */
  targets?: readonly TargetSummary[] | null;
}

export function App({ targets = null }: AppProps) {
  const [client] = useState(createQueryClient);
  return (
    <QueryClientProvider client={client}>
      {/* The window shows once the first screen and the theme are in (shell/reveal.tsx). */}
      <RevealWindow>
        <ThemeController />
        <PlatformGate>
          <TargetsSource value={targets}>
            {/* The toasts belong to the shell: a lock takes them away with it. */}
            <LockGate>
              <ToastProvider>
                <Shell />
              </ToastProvider>
            </LockGate>
          </TargetsSource>
        </PlatformGate>
      </RevealWindow>
    </QueryClientProvider>
  );
}
