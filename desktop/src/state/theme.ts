// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The colour scheme. The page resolves the setting (Rust's `Theme`) against the OS appearance
// and sets `data-theme` on <html>, which tokens.css keys on. The native window (the macOS
// traffic lights, the Linux header bar) gets the explicit theme, or under `system` is left to
// the OS: forcing it would force the webview's prefers-color-scheme as well (tao sets
// NSApp.appearance app-wide on macOS, gtk-application-prefer-dark-theme on Linux), and the page
// would never hear the OS change again.
// Whether WebKitGTK follows the GNOME colour scheme with the theme left to the OS needs a real
// Linux desktop to tell (D.2f manual list).
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { Theme } from '../ipc/generated/Theme';

export type ResolvedTheme = 'light' | 'dark';

const DARK_QUERY = '(prefers-color-scheme: dark)';

export function resolveTheme(setting: Theme, prefersDark: boolean): ResolvedTheme {
  if (setting === 'system') return prefersDark ? 'dark' : 'light';
  return setting;
}

function setPageTheme(resolved: ResolvedTheme) {
  document.documentElement.dataset.theme = resolved;
}

/**
 * Sets the page theme, then the native window's: the explicit theme, or null under `system`.
 * The page goes first, so a refused native call still leaves it right; the returned promise
 * rejects with that refusal.
 */
export async function applyTheme(setting: Theme, prefersDark: boolean): Promise<void> {
  setPageTheme(resolveTheme(setting, prefersDark));
  await getCurrentWindow().setTheme(setting === 'system' ? null : setting);
}

/** Calls `onChange` with the OS's dark preference each time it changes; returns the unsubscribe. */
export function watchSystemTheme(onChange: (prefersDark: boolean) => void): () => void {
  const list = window.matchMedia(DARK_QUERY);
  const listener = (event: MediaQueryListEvent) => onChange(event.matches);
  list.addEventListener('change', listener);
  return () => list.removeEventListener('change', listener);
}

/**
 * Applies `setting` now and, under `system` only, follows each OS appearance change on the page
 * (the window follows the OS by itself). Returns the stop. A refused native call is reported to
 * the console: the page theme is already right.
 */
export function followTheme(setting: Theme): () => void {
  applyTheme(setting, window.matchMedia(DARK_QUERY).matches).catch((error: unknown) => {
    console.error('the window theme was not applied:', error);
  });
  if (setting !== 'system') return () => {};
  return watchSystemTheme((prefersDark) => setPageTheme(resolveTheme(setting, prefersDark)));
}
