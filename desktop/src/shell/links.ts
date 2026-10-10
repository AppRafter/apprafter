// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The app's three links (sidebar footer, Settings › About), opened in the browser through the
// opener. The capability allows exactly these URLs and the install pages the core's tool specs
// name (capabilities/main.json5; tests/ipc_mock.rs reads LINKS from this file and pins both).
import { openUrl } from '@tauri-apps/plugin-opener';

export const LINKS = {
  website: 'https://apprafter.dev',
  docs: 'https://docs.apprafter.dev',
  github: 'https://github.com/AppRafter/apprafter',
} as const;

export function openLink(url: (typeof LINKS)[keyof typeof LINKS]): void {
  openUrl(url).catch((error: unknown) => console.error(`${url} did not open:`, error));
}

/**
 * An install page a tool spec names (the toolchain panel), opened in the browser; never an
 * <a href>, which would navigate the webview itself. The opener refuses any other URL.
 */
export function openInstallPage(url: string): Promise<void> {
  return openUrl(url);
}
