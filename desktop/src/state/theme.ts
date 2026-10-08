// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The colour scheme. The setting (Rust's `Theme`) resolves against the OS appearance here, not
// in CSS, so one answer drives both the page (`data-theme` on <html>, which tokens.css keys on)
// and the native window (the macOS traffic lights, the Linux header bar).
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { Theme } from '../ipc/generated/Theme';

export type ResolvedTheme = 'light' | 'dark';

const DARK_QUERY = '(prefers-color-scheme: dark)';

export function resolveTheme(setting: Theme, prefersDark: boolean): ResolvedTheme {
  if (setting === 'system') return prefersDark ? 'dark' : 'light';
  return setting;
}

/**
 * Sets the page theme, then the native window's. The page goes first, so a refused native call
 * still leaves the page right; the returned promise rejects with that refusal.
 */
export async function applyTheme(resolved: ResolvedTheme): Promise<void> {
  document.documentElement.dataset.theme = resolved;
  await getCurrentWindow().setTheme(resolved);
}

/** Calls `onChange` with the OS's dark preference each time it changes; returns the unsubscribe. */
export function watchSystemTheme(onChange: (prefersDark: boolean) => void): () => void {
  const list = window.matchMedia(DARK_QUERY);
  const listener = (event: MediaQueryListEvent) => onChange(event.matches);
  list.addEventListener('change', listener);
  return () => list.removeEventListener('change', listener);
}

/**
 * Applies `setting` now and, under `system` only, again on each OS appearance change. Returns
 * the stop. A refused native call is reported to the console: the page theme is already right.
 */
export function followTheme(setting: Theme): () => void {
  const apply = (prefersDark: boolean) => {
    applyTheme(resolveTheme(setting, prefersDark)).catch((error: unknown) => {
      console.error('the window theme was not applied:', error);
    });
  };
  apply(window.matchMedia(DARK_QUERY).matches);
  return setting === 'system' ? watchSystemTheme(apply) : () => {};
}
