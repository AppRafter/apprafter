// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The tabs and what the window shows, as a pure reducer (brief §4.4). Opening or switching a
// tab never touches IPC: the CLI's config.yaml default target is not this.

export type Section =
  | 'overview'
  | 'apps'
  | 'approvals'
  | 'backups'
  | 'data'
  | 'network'
  | 'platform'
  | 'nodes'
  | 'target';

/** One open target. The key is stable across a rename, so the tab never remounts. */
export interface TargetTab {
  readonly key: string;
  readonly target: string;
  readonly section: Section;
}

export type View = { readonly kind: 'tab'; readonly key: string } | { readonly kind: 'targets' };

export interface Session {
  readonly tabs: readonly TargetTab[];
  readonly view: View;
}

export const INITIAL_SESSION: Session = { tabs: [], view: { kind: 'targets' } };

export type SessionAction =
  /** Show the target's tab, or open one on Overview (`key` names a new one). */
  | { readonly type: 'openTarget'; readonly target: string; readonly key: string }
  | { readonly type: 'show'; readonly view: View }
  /** Close a tab; when it was shown, its neighbour is (DC closeTab), else the Targets view. */
  | { readonly type: 'closeTab'; readonly key: string }
  | { readonly type: 'navigate'; readonly key: string; readonly section: Section }
  | { readonly type: 'targetRenamed'; readonly from: string; readonly to: string }
  | { readonly type: 'targetRemoved'; readonly target: string };

function close(state: Session, key: string): Session {
  const index = state.tabs.findIndex((tab) => tab.key === key);
  if (index === -1) return state;
  const tabs = state.tabs.filter((tab) => tab.key !== key);
  if (state.view.kind !== 'tab' || state.view.key !== key) return { ...state, tabs };
  const next = tabs[Math.min(index, tabs.length - 1)];
  return { tabs, view: next === undefined ? { kind: 'targets' } : { kind: 'tab', key: next.key } };
}

export function sessionReducer(state: Session, action: SessionAction): Session {
  switch (action.type) {
    case 'openTarget': {
      const open = state.tabs.find((tab) => tab.target === action.target);
      if (open !== undefined) return { ...state, view: { kind: 'tab', key: open.key } };
      const tab: TargetTab = { key: action.key, target: action.target, section: 'overview' };
      return { tabs: [...state.tabs, tab], view: { kind: 'tab', key: tab.key } };
    }
    case 'show': {
      const { view } = action;
      if (view.kind === 'tab' && !state.tabs.some((tab) => tab.key === view.key)) return state;
      return { ...state, view };
    }
    case 'closeTab':
      return close(state, action.key);
    case 'navigate': {
      if (!state.tabs.some((tab) => tab.key === action.key)) return state;
      return {
        ...state,
        tabs: state.tabs.map((tab) =>
          tab.key === action.key ? { ...tab, section: action.section } : tab,
        ),
      };
    }
    case 'targetRenamed': {
      // One tab per target: a tab still bound to the new name (that target removed in a
      // terminal, its tab left open) goes, and the renamed tab takes its place if it was shown.
      const renamed = state.tabs.find((tab) => tab.target === action.from);
      const stale = new Set(
        renamed === undefined
          ? []
          : state.tabs.filter((tab) => tab.target === action.to).map((tab) => tab.key),
      );
      const view: View =
        renamed !== undefined && state.view.kind === 'tab' && stale.has(state.view.key)
          ? { kind: 'tab', key: renamed.key }
          : state.view;
      return {
        tabs: state.tabs
          .filter((tab) => !stale.has(tab.key))
          .map((tab) => (tab.target === action.from ? { ...tab, target: action.to } : tab)),
        view,
      };
    }
    case 'targetRemoved':
      // Every tab of the target, should there be more than one.
      return state.tabs
        .filter((tab) => tab.target === action.target)
        .reduce((next, tab) => close(next, tab.key), state);
  }
}

/** The keys of the tabs in `before` that `after` no longer has. */
export function closedTabs(before: readonly TargetTab[], after: readonly TargetTab[]): string[] {
  const open = new Set(after.map((tab) => tab.key));
  return before.filter((tab) => !open.has(tab.key)).map((tab) => tab.key);
}
