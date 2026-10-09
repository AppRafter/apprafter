// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Locked, the app is the title bar and the lock screen, and nothing else: the shell (its tabs,
// dialogs, toasts) is unmounted, and the cluster data it read leaves the query cache. Unlocked,
// the shell renders, and the owner's activity restarts Rust's idle timer at most once per
// ACTIVITY_INTERVAL_MS. Rust ends every operation subscription on each transition; the
// operations store forgets them, and after an unlock follows again what is still followed.
import { useQueryClient } from '@tanstack/react-query';
import { type ReactNode, useEffect, useRef } from 'react';
import { ErrorPanel } from '../components/ErrorPanel';
import { activity, IpcError, uiErrorOf } from '../ipc/api';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import { clearLive, reattachAll } from '../ipc/operations';
import { ACTIVITY_INTERVAL_MS, dropUnlockedData, useLockState } from '../state/lock';
import { usePlatform } from '../state/platform';
import { LockScreen } from './LockScreen';
import { ScreenShown } from './reveal';
import { TitleBar, Wordmark } from './TitleBar';

export interface LockGateProps {
  children: ReactNode;
  /** The clock the activity pings are throttled by; tests pass their own. */
  now?: () => number;
}

export function LockGate({ children, now = Date.now }: LockGateProps) {
  const { os } = usePlatform();
  const client = useQueryClient();
  const lock = useLockState();
  const locked = lock.data?.locked;

  // Transitions, not events: the unlock's answer and its event change nothing twice.
  const previous = useRef<boolean | undefined>(undefined);
  useEffect(() => {
    if (locked === undefined) return;
    const before = previous.current;
    previous.current = locked;
    if (before === undefined || before === locked) return;
    clearLive();
    if (locked) dropUnlockedData(client);
    else reattachAll();
  }, [locked, client]);

  useEffect(() => {
    if (locked !== false) return;
    let last = Number.NEGATIVE_INFINITY;
    const ping = () => {
      const at = now();
      if (at - last < ACTIVITY_INTERVAL_MS) return;
      last = at;
      activity().catch((error: unknown) => {
        // A lock that landed just before: the gate refuses the ping, and that is expected.
        if (!(error instanceof IpcError && error.error.code === DESKTOP_ERROR_CODES.LOCKED)) {
          console.error('activity failed:', error);
        }
      });
    };
    window.addEventListener('pointerdown', ping, true);
    window.addEventListener('keydown', ping, true);
    // Reading a long page is activity too; passive, so the listener never holds a scroll up.
    window.addEventListener('wheel', ping, { capture: true, passive: true });
    return () => {
      window.removeEventListener('pointerdown', ping, true);
      window.removeEventListener('keydown', ping, true);
      window.removeEventListener('wheel', ping, { capture: true });
    };
  }, [locked, now]);

  if (lock.isPending) return null;
  if (lock.isError) {
    return (
      <div className="app">
        <TitleBar os={os}>
          <Wordmark />
        </TitleBar>
        <div className="app-error">
          <ErrorPanel error={uiErrorOf(lock.error)} />
        </div>
        <ScreenShown />
      </div>
    );
  }
  if (lock.data.locked) {
    return (
      <div className="app">
        <TitleBar os={os}>
          <Wordmark />
        </TitleBar>
        <LockScreen state={lock.data} />
        <ScreenShown />
      </div>
    );
  }
  return children;
}
