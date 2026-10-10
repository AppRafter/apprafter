// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The tab a component renders in, and whether that tab is the one shown.
import { createContext, useContext } from 'react';
import type { TargetTab } from './session';

export interface TabState {
  readonly tab: TargetTab;
  readonly active: boolean;
}

export const TabContext = createContext<TabState | null>(null);

/** The surrounding tab; null outside one (the Targets view, the app's own overlays). */
export function useTab(): TabState | null {
  return useContext(TabContext);
}

/** Whether the surrounding tab is shown; outside a tab, always. */
export function useTabActive(): boolean {
  return useTab()?.active ?? true;
}
