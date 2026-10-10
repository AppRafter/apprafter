// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The colour scheme. The page resolves the setting (Rust's `Theme`) against the OS appearance
// and sets `data-theme` on <html>, which tokens.css keys on. The native window (the macOS
// traffic lights, the Linux header bar) gets the setting through Rust's `theme_apply`
// (src-tauri/src/theme.rs): an explicit theme as it is; under `system`, on macOS and Windows,
// left to the OS, and on Linux resolved from the desktop's colour scheme (the XDG portal) and
// followed while it stays `system`. Never the window plugin's setTheme(null): on Linux tao reads
// it as light and forces it.
// Either way the webview's prefers-color-scheme follows the window's theme (WebKitGTK re-reads
// gtk-application-prefer-dark-theme on every change), so under `system` the page hears each
// change through matchMedia and re-resolves.
import { themeApply } from '../ipc/api';
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
 * Sets the page theme, then has Rust give the native window the setting's (`theme_apply`). The
 * page goes first, so a refused native call still leaves it right; the returned promise rejects
 * with that refusal.
 */
export async function applyTheme(setting: Theme, prefersDark: boolean): Promise<void> {
  setPageTheme(resolveTheme(setting, prefersDark));
  await themeApply(setting);
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
 * (Rust keeps the window following it). Returns the stop. A refused native call is reported to
 * the console: the page theme is already right.
 */
export function followTheme(setting: Theme): () => void {
  applyTheme(setting, window.matchMedia(DARK_QUERY).matches).catch((error: unknown) => {
    console.error('the window theme was not applied:', error);
  });
  if (setting !== 'system') return () => {};
  return watchSystemTheme((prefersDark) => setPageTheme(resolveTheme(setting, prefersDark)));
}
