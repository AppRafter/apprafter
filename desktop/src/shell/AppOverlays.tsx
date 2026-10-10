// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The app's own overlays (the design's root `wizard` and `doctor`, beside Settings): the
// add-target wizard, Doctor, and what they open — the toolchain, the SSH key change and its
// confirm. They render over every view, outside each view's <Activity>: a tab switch would hide
// them and run their effects' cleanups while the wizard still says "Token verified". While one is
// open the Shell makes the tab strip inert and ignores every window shortcut but Lock; the views
// are inert under its layer, and the title bar (the Windows caption buttons) stays live.
//
// Each overlay has a scope (ipc/lifecycle.ts) under the session's: its close ends it — the
// wizard's draft is discarded, what it reads cancelled — and the lock ends them all as it
// unmounts the Shell. An overlay a flow opens from inside one (the doctor's SSH key change, its
// confirm) is an app overlay too: beside it, never inside its form (GOTCHA-144), and above it.
import { createContext, type ReactNode, useCallback, useContext, useRef, useState } from 'react';
import { newScope, type Scope, sessionScope } from '../ipc/lifecycle';
import { ScopeContext } from '../state/scope';
import { OverlayContext, type ShowOverlay } from './ViewFrame';

export const AppOverlayContext = createContext<ShowOverlay | null>(null);

/** Opens an overlay of the app, over every view; `render` gets the function that closes it. */
export function useAppOverlay(): ShowOverlay {
  const show = useContext(AppOverlayContext);
  if (show === null) throw new Error('useAppOverlay is used outside the app overlay host');
  return show;
}

export interface AppOverlayHost {
  readonly show: ShowOverlay;
  /** Whether one is open: the Shell makes the window around it inert. */
  readonly open: boolean;
  /** The open overlays, to render beside the views (their layers then cover them). */
  readonly overlays: ReactNode;
}

interface Overlay {
  readonly id: number;
  readonly scope: Scope;
  readonly node: ReactNode;
}

export function useAppOverlayHost(): AppOverlayHost {
  const [open, setOpen] = useState<readonly Overlay[]>([]);
  const next = useRef(0);
  const show = useCallback<ShowOverlay>((render) => {
    next.current += 1;
    const id = next.current;
    const own = newScope(sessionScope());
    const close = () => {
      own.end();
      setOpen((all) => all.filter((overlay) => overlay.id !== id));
    };
    setOpen((all) => [...all, { id, scope: own.scope, node: render(close) }]);
  }, []);
  return {
    show,
    open: open.length > 0,
    overlays: open.map((overlay) => (
      <ScopeContext key={overlay.id} value={overlay.scope}>
        <OverlayContext value={show}>{overlay.node}</OverlayContext>
      </ScopeContext>
    )),
  };
}
