// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What app_info says about this computer and this build: the OS (per-OS chrome and wording),
// versions, account, authentication. Read once at start (shell/PlatformGate.tsx).
import { createContext, useContext } from 'react';
import type { AppInfo } from '../ipc/generated/AppInfo';
import type { Os } from '../ipc/generated/Os';

export const PlatformContext = createContext<AppInfo | null>(null);

export function usePlatform(): AppInfo {
  const info = useContext(PlatformContext);
  if (info === null) throw new Error('usePlatform is used outside the PlatformGate');
  return info;
}

/**
 * The OS as the webview's user agent tells it, for the one screen app_info cannot inform: its
 * own failure. WebView2 says "Windows", WKWebView "Macintosh"; anything else is taken for Linux.
 */
export function osFromUserAgent(userAgent: string): Os {
  if (userAgent.includes('Windows')) return 'windows';
  if (userAgent.includes('Macintosh') || userAgent.includes('Mac OS X')) return 'macos';
  return 'linux';
}

const OS_NAMES: Record<Os, string> = { windows: 'Windows', macos: 'macOS', linux: 'Linux' };

/** "Windows", "macOS", "Linux": per-OS copy ("System follows the macOS appearance."). */
export function osName(os: Os): string {
  return OS_NAMES[os];
}
