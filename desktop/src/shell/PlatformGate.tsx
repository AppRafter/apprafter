// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// app_info, read once: nothing renders before it (the window's own background shows), and a
// failure is shown as it is.
import { useQuery } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { ErrorPanel } from '../components/ErrorPanel';
import { appInfo, uiErrorOf } from '../ipc/api';
import { PlatformContext } from '../state/platform';

export function PlatformGate({ children }: { children: ReactNode }) {
  const info = useQuery({ queryKey: ['app-info'], queryFn: appInfo, staleTime: Infinity });
  if (info.isPending) return null;
  if (info.isError) {
    return (
      <div className="app-error">
        <ErrorPanel error={uiErrorOf(info.error)} />
      </div>
    );
  }
  return <PlatformContext value={info.data}>{children}</PlatformContext>;
}
