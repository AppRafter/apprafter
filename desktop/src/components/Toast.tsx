// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One toast slot: the newest replaces the one shown and goes after TOAST_MS. The viewport is a
// polite live region that is always there, so a screen reader announces each new message. The
// slot belongs to the shell, which a lock unmounts (App.tsx): a toast that says how a change
// ended and is still in the slot when the app locks is kept for after the unlock (D.4a N5).
import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from 'react';
import { keepEndedAway } from '../ipc/away';
import { sessionScope } from '../ipc/lifecycle';
import { CheckCircleIcon, type Icon } from './icons';

/** How long a toast is shown, unless a newer one takes its place first. */
export const TOAST_MS = 3400;

export interface ToastMessage {
  readonly message: string;
  readonly icon?: Icon;
  /**
   * A failure's error code (WI-483), said under the message as an ErrorPanel says it, and kept
   * with it when a lock keeps the toast (below).
   */
  readonly code?: string;
  /**
   * The toast says how a change ended (D.4a N5): `done`, or `failed` for a failure or a cancel.
   * If the app locks while it is still shown — less than TOAST_MS after it appeared, and no
   * newer toast in its place — the app shows it again after the unlock, as it does an end that
   * arrives while locked (ipc/away.ts). Absent: a toast about anything else (a copy, a refused
   * lock), which a lock takes away for good.
   */
  readonly end?: 'done' | 'failed';
}

/** Runs `run` after `ms`; returns the cancel. Tests pass one they drive by hand. */
export type Schedule = (run: () => void, ms: number) => () => void;

const timeout: Schedule = (run, ms) => {
  const handle = setTimeout(run, ms);
  return () => clearTimeout(handle);
};

type Shown = ToastMessage & { readonly id: number };

const ShowContext = createContext<((toast: ToastMessage) => void) | null>(null);
const CurrentContext = createContext<Shown | null>(null);

export function ToastProvider({
  children,
  schedule = timeout,
}: {
  children: ReactNode;
  schedule?: Schedule;
}) {
  const [current, setCurrent] = useState<Shown | null>(null);
  // The toast in the slot as a lock finds it: set by the call that shows it and cleared when its
  // time is over, never a render later as `current` is.
  const inSlot = useRef<Shown | null>(null);
  const cancel = useRef<(() => void) | null>(null);
  const next = useRef(0);

  const show = useCallback(
    (toast: ToastMessage) => {
      cancel.current?.();
      next.current += 1;
      const id = next.current;
      const shown = { ...toast, id };
      inSlot.current = shown;
      setCurrent(shown);
      cancel.current = schedule(() => {
        if (inSlot.current?.id === id) inSlot.current = null;
        setCurrent((was) => (was?.id === id ? null : was));
      }, TOAST_MS);
    },
    [schedule],
  );

  useEffect(() => () => cancel.current?.(), []);

  // D.4a N5: the lock ends the session (ipc/lifecycle.ts) in the commit that unmounts this slot.
  // An end still in the slot then is kept for the app, which shows it after the unlock; a slot
  // that goes without a lock (the quit) keeps nothing.
  useEffect(
    () =>
      sessionScope().onGone(() => {
        const covered = inSlot.current;
        inSlot.current = null;
        if (covered?.end === undefined) return;
        keepEndedAway({
          opId: null,
          text: covered.message,
          failed: covered.end === 'failed',
          ...(covered.code !== undefined && { code: covered.code }),
        });
      }),
    [],
  );

  return (
    <ShowContext value={show}>
      <CurrentContext value={current}>{children}</CurrentContext>
    </ShowContext>
  );
}

/** Shows a toast; the one shown, if any, goes. */
export function useToast(): (toast: ToastMessage) => void {
  const show = useContext(ShowContext);
  if (show === null) throw new Error('useToast is used outside a ToastProvider');
  return show;
}

/** Where the toast shows: bottom right of its positioned parent. */
export function ToastViewport() {
  const current = useContext(CurrentContext);
  const ToastIcon = current?.icon ?? CheckCircleIcon;
  return (
    <div className="toast-viewport" role="status" aria-live="polite">
      {current !== null && (
        <div className="toast" key={current.id}>
          <ToastIcon className="toast-icon" aria-hidden="true" />
          <span className="toast-text">
            <span>{current.message}</span>
            {current.code !== undefined && <span className="toast-code">{current.code}</span>}
          </span>
        </div>
      )}
    </div>
  );
}
