// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One view (a target tab, or the Targets view): its content, and the overlays opened from it.
// The overlays render beside the content in this positioned frame, so a modal covers the view
// only, makes the content inert, and hides with its tab (brief §4.3: a "Remove target" confirm
// must never stay over another tab). Each view is a stacking context of its own (shell.css), so
// its dialogs stay under Settings, which covers every view. Each overlay has a scope of its own
// (ipc/lifecycle.ts) under the view's: its close ends it, so what the overlay started (a read, a
// held plan) goes with it — and with its view, when the tab closes.
import { createContext, type ReactNode, useCallback, useContext, useRef, useState } from 'react';
import { newScope, type Scope } from '../ipc/lifecycle';
import { ScopeContext, useScope } from '../state/scope';

/** Opens an overlay; `render` gets the function that closes it. */
export type ShowOverlay = (render: (close: () => void) => ReactNode) => void;

/** The overlay host `useOverlay` opens in: a ViewFrame's, or inside an app overlay the app's. */
export const OverlayContext = createContext<ShowOverlay | null>(null);

export function useOverlay(): ShowOverlay {
  const show = useContext(OverlayContext);
  if (show === null) throw new Error('useOverlay is used outside a ViewFrame');
  return show;
}

export interface ViewFrameProps {
  /** A tab's view is the panel its tab controls: its id, and the tab's that names it. */
  panel?: { readonly id: string; readonly labelledBy: string };
  children: ReactNode;
}

export function ViewFrame({ panel, children }: ViewFrameProps) {
  const view = useScope();
  const [overlays, setOverlays] = useState<readonly Overlay[]>([]);
  const next = useRef(0);
  const show = useCallback<ShowOverlay>(
    (render) => {
      next.current += 1;
      const id = next.current;
      const own = newScope(view);
      const close = () => {
        own.end();
        setOverlays((open) => open.filter((overlay) => overlay.id !== id));
      };
      setOverlays((open) => [...open, { id, scope: own.scope, node: render(close) }]);
    },
    [view],
  );
  return (
    <OverlayContext value={show}>
      <div
        className="view"
        {...(panel && { role: 'tabpanel', id: panel.id, 'aria-labelledby': panel.labelledBy })}
      >
        <div className="view-content">{children}</div>
        {overlays.map((overlay) => (
          <ScopeContext key={overlay.id} value={overlay.scope}>
            {overlay.node}
          </ScopeContext>
        ))}
      </div>
    </OverlayContext>
  );
}

interface Overlay {
  readonly id: number;
  readonly scope: Scope;
  readonly node: ReactNode;
}
