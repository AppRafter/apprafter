// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The open targets as tabs (brief §2): health dot (unknown until the notifier measures it,
// D.5), name, approvals badge (hidden at zero), close; and "+" for the Targets view. A spinner
// replaces the dot while an operation runs on that target.
//
// A tab is 184px and shrinks to 96px when the strip is full; past that the strip scrolls
// sideways, and the shown tab is scrolled into view, so every tab stays reachable and its close
// button usable. The keyboard follows the ARIA tabs pattern: one tab stop (the shown tab, or
// the first while the Targets view shows), the arrow keys, Home and End move to a tab and show
// it, wrapping at the ends, and Delete closes the focused tab — the close buttons are for the
// pointer and are no Tab stops — handing the focus to the tab that follows it (the one before
// it for the last, "+" for the only one). Mod+W is not offered: on macOS the app menu's Close
// Window holds it. Each tab controls its view, the tab panel with tabPanelId(key).
import { type KeyboardEvent, useEffect, useRef } from 'react';
import { Badge } from '../components/Badge';
import { Dot } from '../components/Dot';
import { IconButton } from '../components/IconButton';
import { PlusIcon, SpinnerGapIcon, XIcon } from '../components/icons';
import type { Os } from '../ipc/generated/Os';
import type { TargetTab, View } from '../state/session';
import { shortcutHint } from '../state/shortcuts';

/** The id of a tab's tab element, and of the panel (its view) it controls. */
export const tabId = (key: string) => `tab-${key}`;
export const tabPanelId = (key: string) => `tabpanel-${key}`;

export interface TabStripProps {
  tabs: readonly TargetTab[];
  view: View;
  os: Os;
  /** Targets with an operation running. */
  running: ReadonlySet<string>;
  /** Approvals waiting per target (the notifier, D.5); none until then. */
  approvals?: (target: string) => number;
  onShow: (view: View) => void;
  onClose: (key: string) => void;
  onNewTab: () => void;
  /** An app overlay covers the window: the strip takes no focus and no click. */
  inert?: boolean;
}

export function TabStrip({
  tabs,
  view,
  os,
  running,
  approvals = () => 0,
  onShow,
  onClose,
  onNewTab,
  inert = false,
}: TabStripProps) {
  const elements = useRef(new Map<string, HTMLButtonElement>());
  const plus = useRef<HTMLButtonElement>(null);
  /** A tab Delete closed, and the tab the focus goes to once it is gone (null: "+"). */
  const closing = useRef<{ key: string; next: string | null } | null>(null);
  const shown = view.kind === 'tab' ? view.key : null;
  const stop = shown ?? tabs[0]?.key ?? null;

  useEffect(() => {
    if (shown !== null) {
      elements.current.get(shown)?.scrollIntoView?.({ block: 'nearest', inline: 'nearest' });
    }
  }, [shown]);

  useEffect(() => {
    const closed = closing.current;
    if (closed === null || tabs.some((tab) => tab.key === closed.key)) return;
    closing.current = null;
    const next = closed.next === null ? undefined : elements.current.get(closed.next);
    (next ?? plus.current)?.focus();
  }, [tabs]);

  const onKeyDown = (event: KeyboardEvent, index: number) => {
    if (event.key === 'Delete') {
      const tab = tabs[index];
      if (tab === undefined) return;
      event.preventDefault();
      closing.current = { key: tab.key, next: (tabs[index + 1] ?? tabs[index - 1])?.key ?? null };
      onClose(tab.key);
      return;
    }
    const last = tabs.length - 1;
    const target = {
      ArrowRight: index === last ? 0 : index + 1,
      ArrowLeft: index === 0 ? last : index - 1,
      Home: 0,
      End: last,
    }[event.key];
    const tab = target === undefined ? undefined : tabs[target];
    if (tab === undefined) return;
    event.preventDefault();
    elements.current.get(tab.key)?.focus();
    onShow({ kind: 'tab', key: tab.key });
  };

  return (
    <div className="tabstrip" inert={inert || undefined}>
      <div className="tabs" role="tablist" aria-label="Open clusters">
        {tabs.map((tab, index) => {
          const selected = tab.key === shown;
          return (
            <div key={tab.key} className="tab" data-selected={selected || undefined}>
              <button
                ref={(element) => {
                  if (element === null) elements.current.delete(tab.key);
                  else elements.current.set(tab.key, element);
                }}
                type="button"
                role="tab"
                id={tabId(tab.key)}
                aria-selected={selected}
                aria-controls={tabPanelId(tab.key)}
                tabIndex={tab.key === stop ? 0 : -1}
                className="tab-main"
                onClick={() => onShow({ kind: 'tab', key: tab.key })}
                onKeyDown={(event) => onKeyDown(event, index)}
              >
                {running.has(tab.target) ? (
                  <span className="tab-status" role="img" aria-label="An operation is running">
                    <SpinnerGapIcon className="spin" aria-hidden="true" />
                  </span>
                ) : (
                  <Dot />
                )}
                <span className="tab-name">{tab.target}</span>
                <Badge count={approvals(tab.target)} label="approvals waiting" />
              </button>
              <IconButton
                label={`Close ${tab.target}`}
                icon={XIcon}
                size={22}
                tabIndex={-1}
                onClick={(event) => {
                  event.stopPropagation();
                  onClose(tab.key);
                }}
              />
            </div>
          );
        })}
      </div>
      <IconButton
        ref={plus}
        label="Open a cluster"
        title={`Open a cluster (${shortcutHint('targets', os)})`}
        icon={PlusIcon}
        size={30}
        pressed={view.kind === 'targets'}
        onClick={onNewTab}
      />
    </div>
  );
}
