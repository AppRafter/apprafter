// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// app_info: nothing renders before its first answer (the window is still hidden), and a failure
// of that first read is shown as it is, under the title bar — on Windows the window has no
// decorations, so the bar is what moves it and closes it. The OS that bar is drawn for comes from
// the user agent there. Read again where it may have changed (state/platform.ts): after every
// authentication refusal, heard here; a later read that fails keeps what the first said, without
// the password field, and is logged.
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { type ReactNode, useEffect, useMemo } from 'react';
import { ErrorPanel } from '../components/ErrorPanel';
import { onAuthRefusal, uiErrorOf } from '../ipc/api';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import {
  appInfoQuery,
  osFromUserAgent,
  PlatformContext,
  rereadAppInfo,
  withoutStaleField,
} from '../state/platform';
import { ScreenShown } from './reveal';
import { TitleBar, Wordmark } from './TitleBar';

export function PlatformGate({ children }: { children: ReactNode }) {
  const client = useQueryClient();
  const info = useQuery(appInfoQuery);
  const { data, isStale, error } = info;
  const known = useMemo(
    () => (data === undefined ? undefined : withoutStaleField(data, isStale)),
    [data, isStale],
  );

  useEffect(
    () =>
      onAuthRefusal((refusal) =>
        rereadAppInfo(client, refusal.code === DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE),
      ),
    [client],
  );

  const laterFailure = data !== undefined ? error : null;
  useEffect(() => {
    if (laterFailure !== null) console.error('app_info could not be read again:', laterFailure);
  }, [laterFailure]);

  if (info.isPending) return null;
  if (known === undefined) {
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
  return <PlatformContext value={known}>{children}</PlatformContext>;
}
