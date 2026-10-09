// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// app_info, read once: nothing renders before it (the window is still hidden), and a failure is
// shown as it is, under the title bar — on Windows the window has no decorations, so the bar is
// what moves it and closes it. The OS that bar is drawn for comes from the user agent there.
import { useQuery } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { ErrorPanel } from '../components/ErrorPanel';
import { appInfo, uiErrorOf } from '../ipc/api';
import { osFromUserAgent, PlatformContext } from '../state/platform';
import { ScreenShown } from './reveal';
import { TitleBar, Wordmark } from './TitleBar';

export function PlatformGate({ children }: { children: ReactNode }) {
  const info = useQuery({ queryKey: ['app-info'], queryFn: appInfo, staleTime: Infinity });
  if (info.isPending) return null;
  if (info.isError) {
    return (
      <div className="app">
        <TitleBar os={osFromUserAgent(navigator.userAgent)}>
          <Wordmark />
        </TitleBar>
        <div className="app-error">
          <ErrorPanel error={uiErrorOf(info.error)} />
        </div>
        <ScreenShown />
      </div>
    );
  }
  return <PlatformContext value={info.data}>{children}</PlatformContext>;
}
