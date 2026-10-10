// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A quit with operations running cancels them and waits, up to its bound, before the app
// exits; the window stays meanwhile, and every command is refused. So once Rust says the quit
// began (`quitting`), the app is the title bar and what it waits for, whatever showed before —
// the shell or the lock screen — and nothing else.
import { type ReactNode, useEffect, useState } from 'react';
import { SpinnerGapIcon } from '../components/icons';
import { StatePanel } from '../components/StatePanel';
import { onQuitting } from '../ipc/events';
import type { Quitting } from '../ipc/generated/Quitting';
import { usePlatform } from '../state/platform';
import { TitleBar, Wordmark } from './TitleBar';

export function QuitGate({ children }: { children: ReactNode }) {
  const { os } = usePlatform();
  const [quitting, setQuitting] = useState<Quitting | null>(null);

  useEffect(() => {
    let mounted = true;
    let off: (() => void) | undefined;
    onQuitting((quit) => setQuitting(quit))
      .then((unlisten) => {
        if (mounted) off = unlisten;
        else unlisten();
      })
      .catch((error: unknown) => console.error('quitting is not heard:', error));
    return () => {
      mounted = false;
      off?.();
    };
  }, []);

  if (quitting === null) return children;
  const one = quitting.running === 1;
  const seconds = Math.round(quitting.waitMs / 1000);
  return (
    <div className="app">
      <TitleBar os={os}>
        <Wordmark />
      </TitleBar>
      <div className="app-error" role="status">
        <StatePanel
          icon={SpinnerGapIcon}
          spin
          title={one ? 'Stopping 1 operation…' : `Stopping ${quitting.running} operations…`}
          text={`AppRafter quits once ${one ? 'it has' : 'they have'} stopped, within ${seconds} seconds.`}
        />
      </div>
    </div>
  );
}
