// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One view (a target tab, or the Targets view): its content, and the overlays opened from it.
// The overlays render beside the content in this positioned frame, so a modal covers the view
// only, makes the content inert, and hides with its tab (brief §4.3: a "Remove target" confirm
// must never stay over another tab).
import {
  createContext,
  Fragment,
  type ReactNode,
  useCallback,
  useContext,
  useRef,
  useState,
} from 'react';

/** Opens an overlay; `render` gets the function that closes it. */
export type ShowOverlay = (render: (close: () => void) => ReactNode) => void;

const OverlayContext = createContext<ShowOverlay | null>(null);

export function useOverlay(): ShowOverlay {
  const show = useContext(OverlayContext);
  if (show === null) throw new Error('useOverlay is used outside a ViewFrame');
  return show;
}

export function ViewFrame({ children }: { children: ReactNode }) {
  const [overlays, setOverlays] = useState<readonly { id: number; node: ReactNode }[]>([]);
  const next = useRef(0);
  const show = useCallback<ShowOverlay>((render) => {
    next.current += 1;
    const id = next.current;
    const close = () => setOverlays((open) => open.filter((overlay) => overlay.id !== id));
    setOverlays((open) => [...open, { id, node: render(close) }]);
  }, []);
  return (
    <OverlayContext value={show}>
      <div className="view">
        <div className="view-content">{children}</div>
        {overlays.map((overlay) => (
          <Fragment key={overlay.id}>{overlay.node}</Fragment>
        ))}
      </div>
    </OverlayContext>
  );
}
