// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One toast slot: the newest replaces the one shown and goes after TOAST_MS. The viewport is a
// polite live region that is always there, so a screen reader announces each new message.
import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from 'react';
import { CheckCircleIcon, type Icon } from './icons';

export const TOAST_MS = 3400;

export interface ToastMessage {
  readonly message: string;
  readonly icon?: Icon;
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
  const cancel = useRef<(() => void) | null>(null);
  const next = useRef(0);

  const show = useCallback(
    (toast: ToastMessage) => {
      cancel.current?.();
      next.current += 1;
      const id = next.current;
      setCurrent({ ...toast, id });
      cancel.current = schedule(
        () => setCurrent((shown) => (shown?.id === id ? null : shown)),
        TOAST_MS,
      );
    },
    [schedule],
  );

  useEffect(() => () => cancel.current?.(), []);

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
          <span>{current.message}</span>
        </div>
      )}
    </div>
  );
}
