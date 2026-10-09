// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What app_info says about this computer and this build: the OS (per-OS chrome and wording),
// versions, account, authentication, the session's signals. Read at start (shell/PlatformGate),
// and again wherever what it says may have changed: Rust's AuthInfo is live — on Linux the
// password field appears once polkit finds no agent, and every lock forgets that again — and a
// session watch that answers late says more in a later read.
//
// Read again: on every lock (shell/LockGate), after every authentication refusal from any
// command (PlatformGate, through onAuthRefusal), and when Settings opens. While a read is in
// question — after a lock or `auth_unavailable`, until app_info answers — the password field is
// not offered: never one that answers `not_permitted_here`. After `auth_failed` or `auth_busy`
// the field the owner types in stays while app_info is read.
import { type QueryClient, queryOptions } from '@tanstack/react-query';
import { createContext, useContext } from 'react';
import { appInfo } from '../ipc/api';
import type { AppInfo } from '../ipc/generated/AppInfo';
import type { Os } from '../ipc/generated/Os';

export const APP_INFO_KEY = ['app-info'] as const;

/** The app_info query; never stale by time — read again on the occasions above. */
export const appInfoQuery = queryOptions({
  queryKey: APP_INFO_KEY,
  queryFn: appInfo,
  staleTime: Infinity,
});

/**
 * Read app_info again. `inQuestion`: what it said of the password field may be wrong now, and
 * the field is withheld until it answers (withoutStaleField); otherwise it stays meanwhile.
 */
export function rereadAppInfo(client: QueryClient, inQuestion: boolean): void {
  const filters = { queryKey: APP_INFO_KEY, exact: true };
  // Neither rejects: a failed read is the query's error, and its data is kept.
  void (inQuestion ? client.invalidateQueries(filters) : client.refetchQueries(filters));
}

/** `info` as the page may act on it: no password field while a read of it is in question. */
export function withoutStaleField(info: AppInfo, inQuestion: boolean): AppInfo {
  if (!inQuestion || !info.auth.passwordField) return info;
  return { ...info, auth: { ...info.auth, passwordField: false } };
}

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
