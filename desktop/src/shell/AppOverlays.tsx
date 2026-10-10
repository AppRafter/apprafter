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
// unmounts the Shell. An overlay a flow opens from inside one (the doctor's toolchain, its SSH key
// change and that change's confirm) is an app overlay too: beside it, never inside its form
// (GOTCHA-144), and above it — and its scope is under its opener's, so it goes when its opener
// goes, however that goes. Inside an overlay, useAppOverlay and useOverlay open such children;
// one opened through an overlay that has gone opens nothing.
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
  /** Opens an overlay of this one's: above it, and gone with it. */
  readonly showHere: ShowOverlay;
}

export function useAppOverlayHost(): AppOverlayHost {
  const [open, setOpen] = useState<readonly Overlay[]>([]);
  const next = useRef(0);
  const openUnder = useCallback((parent: Scope, render: Parameters<ShowOverlay>[0]) => {
    const own = newScope(parent);
    // Opened through an overlay that has gone: nothing opens.
    if (own.scope.gone()) return;
    next.current += 1;
    const id = next.current;
    // Off the screen when its scope ends: its close, its opener's going, or the lock.
    own.scope.onGone(() => setOpen((all) => all.filter((overlay) => overlay.id !== id)));
    const showHere: ShowOverlay = (child) => openUnder(own.scope, child);
    setOpen((all) => [...all, { id, scope: own.scope, node: render(own.end), showHere }]);
  }, []);
  // From the views: under the session that is unlocked when it opens.
  const show = useCallback<ShowOverlay>((render) => openUnder(sessionScope(), render), [openUnder]);
  return {
    show,
    open: open.length > 0,
    overlays: open.map((overlay) => (
      <ScopeContext key={overlay.id} value={overlay.scope}>
        <AppOverlayContext value={overlay.showHere}>
          <OverlayContext value={overlay.showHere}>{overlay.node}</OverlayContext>
        </AppOverlayContext>
      </ScopeContext>
    )),
  };
}
