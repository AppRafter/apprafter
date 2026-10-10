// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Locked, the app is the title bar and the lock screen, and nothing else: the shell (its tabs,
// dialogs, toasts) is unmounted, and the cluster data it read leaves the query cache. Every lock
// reads app_info again, the password field withheld until it answers: Rust forgets a missing
// polkit agent on each lock, so the field the last unlock used may answer `use_system_prompt`
// now. The withholding starts in the very render that shows the lock (LockedScreen), not when
// the re-read's news reaches the PlatformGate a task later: the old field is never painted, nor
// focused. Unlocked, the shell renders, and the owner's activity restarts Rust's idle timer at
// most once per ACTIVITY_INTERVAL_MS. Rust ends every operation subscription on each transition;
// the operations store forgets them, and after an unlock follows again what is still followed.
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { type ReactNode, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { ErrorPanel } from '../components/ErrorPanel';
import { activity, IpcError, uiErrorOf } from '../ipc/api';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { LockState } from '../ipc/generated/LockState';
import { sessionLocked } from '../ipc/lifecycle';
import { clearLive, reattachAll } from '../ipc/operations';
import { ACTIVITY_INTERVAL_MS, dropUnlockedData, useLockState } from '../state/lock';
import {
  appInfoQuery,
  PlatformContext,
  rereadAppInfo,
  usePlatform,
  withoutStaleField,
} from '../state/platform';
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

  // Whether the lock on screen came from the unlocked app, decided in the render that shows it:
  // the layout effect below then asks app_info again, and the field waits for that answer.
  const [seen, setSeen] = useState({ locked, afterUnlocked: false });
  let afterUnlocked = seen.afterUnlocked;
  if (seen.locked !== locked) {
    afterUnlocked = locked === true && seen.locked === false;
    setSeen({ locked, afterUnlocked });
  }

  // Transitions, not events: the unlock's answer and its event change nothing twice. A layout
  // effect, so it runs before the shell's own (passive) effects in the commit that mounts it:
  // what the shell follows as it mounts is not ended and followed again — and before the lock
  // screen paints, so a field from before the lock is never seen.
  const previous = useRef<boolean | undefined>(undefined);
  useLayoutEffect(() => {
    if (locked === undefined) return;
    const before = previous.current;
    previous.current = locked;
    if (before === undefined || before === locked) return;
    clearLive();
    if (locked) {
      // Every screen of the session is gone (ipc/lifecycle.ts): what they started is cancelled or
      // discarded — refused as locked, since Rust has done it already — and a plan that ends
      // after the unlock shows at the app level.
      sessionLocked();
      dropUnlockedData(client);
      rereadAppInfo(client, true);
    } else {
      reattachAll();
    }
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
        <LockedScreen state={lock.data} afterUnlocked={afterUnlocked} />
        <ScreenShown />
      </div>
    );
  }
  return children;
}

/**
 * The lock screen, without the password field until app_info has answered since it was shown,
 * when the lock came from the unlocked app (`afterUnlocked`): what the PlatformGate holds is
 * from before the lock until then. Mounted in the commit that shows the lock, before the gate's
 * layout effect asks app_info again, so its observer counts that read; a read that fails counts
 * too, and the PlatformGate then withholds the field itself. A lock the app starts in shows what
 * app_info has just said.
 */
function LockedScreen({ state, afterUnlocked }: { state: LockState; afterUnlocked: boolean }) {
  const info = usePlatform();
  // Only to hear the read land: the PlatformGate's own observer does the reading.
  const { isFetchedAfterMount } = useQuery({ ...appInfoQuery, enabled: false });
  const shown = useMemo(
    () => withoutStaleField(info, afterUnlocked && !isFetchedAfterMount),
    [info, afterUnlocked, isFetchedAfterMount],
  );
  return (
    <PlatformContext value={shown}>
      <LockScreen state={state} />
    </PlatformContext>
  );
}
