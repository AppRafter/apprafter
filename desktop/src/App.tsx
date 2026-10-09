// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { QueryClientProvider } from '@tanstack/react-query';
import { useState } from 'react';
import { ToastProvider } from './components/Toast';
import { LockGate } from './shell/LockGate';
import { PlatformGate } from './shell/PlatformGate';
import { QuitGate } from './shell/QuitGate';
import { RevealWindow } from './shell/reveal';
import { Shell } from './shell/Shell';
import { ThemeController } from './shell/ThemeController';
import { createQueryClient } from './state/queryClient';

export function App() {
  const [client] = useState(createQueryClient);
  return (
    <QueryClientProvider client={client}>
      {/* The window shows once the first screen and the theme are in (shell/reveal.tsx). */}
      <RevealWindow>
        <ThemeController />
        <PlatformGate>
          {/* A quit waiting for operations replaces whatever shows, the lock screen too. */}
          <QuitGate>
            {/* The toasts belong to the shell: a lock takes them away with it. */}
            <LockGate>
              <ToastProvider>
                <Shell />
              </ToastProvider>
            </LockGate>
          </QuitGate>
        </PlatformGate>
      </RevealWindow>
    </QueryClientProvider>
  );
}
