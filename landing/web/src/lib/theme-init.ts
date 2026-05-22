// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

/**
 * Inline script that runs before paint to set data-theme on <html>
 * and prevent FOUC between dark and light.
 *
 * Logic:
 *   1. read localStorage['apprafter-theme']         — explicit user preference
 *   2. fallback to matchMedia('(prefers-color-scheme: light)') === light
 *   3. fallback to dark                              — per BRIEF §3.6
 *
 * Serialised verbatim into BaseLayout.astro via
 * <script is:inline set:html={THEME_INIT_SCRIPT} />.
 */
export const THEME_INIT_SCRIPT = `
(function () {
  try {
    var stored = localStorage.getItem('apprafter-theme');
    var theme = stored || (window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark');
    document.documentElement.setAttribute('data-theme', theme);
  } catch (e) {
    document.documentElement.setAttribute('data-theme', 'dark');
  }
})();
`;
