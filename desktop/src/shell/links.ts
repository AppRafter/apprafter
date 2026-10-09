// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The app's three links (sidebar footer, Settings › About), opened in the browser through the
// opener; the capability allows exactly these URLs (capabilities/main.json5).
import { openUrl } from '@tauri-apps/plugin-opener';

export const LINKS = {
  website: 'https://apprafter.dev',
  docs: 'https://docs.apprafter.dev',
  github: 'https://github.com/AppRafter/apprafter',
} as const;

export function openLink(url: (typeof LINKS)[keyof typeof LINKS]): void {
  openUrl(url).catch((error: unknown) => console.error(`${url} did not open:`, error));
}
