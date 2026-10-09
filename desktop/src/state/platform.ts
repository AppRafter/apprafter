// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What app_info says about this computer and this build: the OS (per-OS chrome and wording),
// versions, account, authentication. Read once at start (shell/PlatformGate.tsx).
import { createContext, useContext } from 'react';
import type { AppInfo } from '../ipc/generated/AppInfo';

export const PlatformContext = createContext<AppInfo | null>(null);

export function usePlatform(): AppInfo {
  const info = useContext(PlatformContext);
  if (info === null) throw new Error('usePlatform is used outside the PlatformGate');
  return info;
}
