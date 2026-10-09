// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Overlays are positioned divs, not <dialog>.showModal(): its top layer would cover the Windows
// caption buttons, and an overlay belongs to its tab (it hides with the tab). The layer fills
// its positioned parent; while it is open its siblings there are `inert`, a live region apart.
import {
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  useId,
  useLayoutEffect,
  useRef,
} from 'react';
import { IconButton } from './IconButton';
import { type Icon, XIcon } from './icons';

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), ' +
  'textarea:not([disabled]), [tabindex]';

function focusables(root: HTMLElement | null): HTMLElement[] {
  if (root === null) return [];
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (element) =>
      element.getAttribute('tabindex') !== '-1' &&
      !(element instanceof HTMLButtonElement && element.disabled),
  );
}

export interface ModalFrameProps {
  /** The id of the element that names the dialog. */
  labelledBy: string;
  describedBy?: string;
  width: number;
  /** The z-index tier: Settings and wizards, Doctor, Form and Info, Confirm. */
  layer?: 'dialog' | 'doctor' | 'form' | 'confirm';
  onClose: () => void;
  /** False while a submit runs: Esc and the backdrop do nothing. */
  closable?: boolean;
  dismissOnBackdrop?: boolean;
  /** The panel's layout: a header bar (`sheet`) or icon and title in the body (`alert`). */
  variant?: 'sheet' | 'alert';
  children: ReactNode;
}

/**
 * The behaviour every overlay shares: focus moves in (to the first control of the body, else the
 * first control, else the panel), Tab and Shift+Tab wrap inside, Esc closes, the background is
 * inert, and on close the focus returns to where it was.
 */
export function ModalFrame({
  labelledBy,
  describedBy,
  width,
  layer = 'dialog',
  onClose,
  closable = true,
  dismissOnBackdrop = false,
  variant = 'sheet',
  children,
}: ModalFrameProps) {
  const layerRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const pressedOnBackdrop = useRef(false);
  const close = () => {
    if (closable) onClose();
  };

  useLayoutEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const own = layerRef.current;
    const madeInert: Element[] = [];
    for (const sibling of own?.parentElement?.children ?? []) {
      // A live region (the toasts) stays live: what it announces must be heard over the dialog
      // — a refused save in Settings among it. It holds no controls to reach past the modal.
      if (sibling !== own && !sibling.hasAttribute('inert') && !sibling.hasAttribute('aria-live')) {
        sibling.setAttribute('inert', '');
        madeInert.push(sibling);
      }
    }
    const panel = panelRef.current;
    const body = panel?.querySelector<HTMLElement>('[data-modal-body]') ?? null;
    (focusables(body)[0] ?? focusables(panel)[0] ?? panel)?.focus();
    return () => {
      for (const element of madeInert) element.removeAttribute('inert');
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === 'Escape') {
      event.stopPropagation();
      close();
      return;
    }
    if (event.key !== 'Tab') return;
    const items = focusables(panelRef.current);
    const first = items[0];
    const last = items.at(-1);
    if (first === undefined || last === undefined) {
      event.preventDefault();
      return;
    }
    const inside = items.includes(document.activeElement as HTMLElement);
    if (event.shiftKey && (!inside || document.activeElement === first)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (!inside || document.activeElement === last)) {
      event.preventDefault();
      first.focus();
    }
  };

  const onBackdrop = (event: MouseEvent) => {
    const onLayer = event.target === event.currentTarget;
    if (event.type === 'mousedown') {
      pressedOnBackdrop.current = onLayer;
      // A press on the backdrop would move the focus to <body>, out of reach of Esc and Tab.
      if (onLayer) event.preventDefault();
    } else if (dismissOnBackdrop && onLayer && pressedOnBackdrop.current) {
      close();
    }
  };

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: the backdrop; the dialog inside takes the keyboard (Esc closes it)
    <div
      ref={layerRef}
      className="modal-layer"
      data-layer={layer}
      onKeyDown={onKeyDown}
      onMouseDown={onBackdrop}
      onClick={onBackdrop}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelledBy}
        aria-describedby={describedBy}
        tabIndex={-1}
        className="modal"
        data-variant={variant}
        style={{ width }}
      >
        {children}
      </div>
    </div>
  );
}

export interface ModalProps {
  title: string;
  sub?: ReactNode;
  icon?: Icon;
  width: number;
  layer?: ModalFrameProps['layer'];
  onClose: () => void;
  dismissOnBackdrop?: boolean;
  footer?: ReactNode;
  children: ReactNode;
}

/** A dialog with a header bar (title, sub, close button), a scrolling body and a footer. */
export function Modal({
  title,
  sub,
  icon: TitleIcon,
  width,
  layer,
  onClose,
  dismissOnBackdrop = false,
  footer,
  children,
}: ModalProps) {
  const titleId = useId();
  return (
    <ModalFrame
      labelledBy={titleId}
      width={width}
      layer={layer ?? 'dialog'}
      onClose={onClose}
      dismissOnBackdrop={dismissOnBackdrop}
    >
      <div className="modal-head">
        {TitleIcon && <TitleIcon className="modal-icon" aria-hidden="true" />}
        <div className="modal-titles">
          <h2 className="modal-title" id={titleId}>
            {title}
          </h2>
          {sub !== undefined && <div className="modal-sub">{sub}</div>}
        </div>
        <IconButton label="Close" icon={XIcon} onClick={onClose} />
      </div>
      <div className="modal-body" data-modal-body>
        {children}
      </div>
      {footer !== undefined && <div className="modal-foot">{footer}</div>}
    </ModalFrame>
  );
}
