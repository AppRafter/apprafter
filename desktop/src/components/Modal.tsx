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

/**
 * Where the focus goes when a dialog closes and the control that opened it is gone (an error
 * panel's action that cleared the panel, a screen that re-rendered): the page heading of the
 * view the dialog belongs to (`.view` > `.view-content`), never <body>, where the next Tab
 * starts over from the title bar. An app-level dialog (Settings) has no view, and no fallback.
 */
function headingOf(host: Element | null | undefined): HTMLElement | null {
  return host?.querySelector<HTMLElement>(':scope > .view-content h1[tabindex="-1"]') ?? null;
}

const isRadio = (element: Element): element is HTMLInputElement =>
  element instanceof HTMLInputElement && element.type === 'radio';

/**
 * The Tab stops inside `root`, in order. A radio group is one stop, as a browser tabs through it:
 * its checked radio, or its first enabled one when none is checked (review #15) — so the trap's
 * first and last are the stops the browser really moves between. Exported for the Wizard frame,
 * which starts a step's focus at its first stop.
 */
export function focusables(root: HTMLElement | null): HTMLElement[] {
  if (root === null) return [];
  const all = [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (element) =>
      element.getAttribute('tabindex') !== '-1' &&
      !(element instanceof HTMLButtonElement && element.disabled),
  );
  const stopOf = new Map<string, HTMLInputElement>();
  for (const element of all) {
    if (!isRadio(element) || element.name === '') continue;
    const stop = stopOf.get(element.name);
    if (stop === undefined || (element.checked && !stop.checked)) stopOf.set(element.name, element);
  }
  return all.filter(
    (element) => !isRadio(element) || element.name === '' || stopOf.get(element.name) === element,
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
 * inert, and on close the focus returns to where it was — or, that control gone, to its view's
 * page heading.
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
    const opener =
      document.activeElement instanceof HTMLElement && document.activeElement !== document.body
        ? document.activeElement
        : null;
    const own = layerRef.current;
    const host = own?.parentElement;
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
      (opener?.isConnected ? opener : headingOf(host))?.focus();
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
