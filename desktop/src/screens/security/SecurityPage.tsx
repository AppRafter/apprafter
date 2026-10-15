// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Security page (WI-476; D.4a §3.14; ADR 0068 §8, reworded 2026-10-14): the target store's
// protection, as a view of its own beside Targets — the store and every target it covers (the
// lock is the whole store's: one vault, and what opens it opens them all), where its secrets are
// and each state's way on, what opens the vault, and this computer's limits. Settings › Lock
// keeps how the app locks. The Shell keys the page by its count of show requests, so every
// request mounts it anew, the view shown already included; an <Activity> reveal runs its effects
// again too. Each reads app_info again, as opening Settings did, and lock_info and target_list
// (stale at once), and takes the focus to the title: what showed the page is in a view now
// hidden, in Settings, now closed, or in this view's sidebar. A passive mount effect runs after
// a closing Modal's layout cleanup has given the focus back to its opener, so the title wins.
// The flows open as the app's own overlays, over every view and outside its <Activity>
// (GOTCHA-158): a view switch never hides a Protect wizard or runs its cleanups, and a plan a
// flow holds goes when its overlay closes or the app locks.
import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';
import { PageHeader } from '../../components/PageHeader';
import { uiErrorOf } from '../../ipc/api';
import { useAppOverlay } from '../../shell/AppOverlays';
import { useLockInfo } from '../../state/lockInfo';
import { type ProtectAction, SECURITY_SUB, SECURITY_TITLE } from '../../state/lockText';
import { rereadAppInfo, usePlatform } from '../../state/platform';
import { storeTargetNames, useTargetList } from '../../state/targets';
import { LockFlow } from './LockFlow';
import { StoreProtection, StoreScope } from './StoreProtection';

export function SecurityPage() {
  const info = usePlatform();
  const client = useQueryClient();
  const report = useLockInfo();
  const list = useTargetList();
  const showApp = useAppOverlay();
  const page = useRef<HTMLDivElement>(null);
  useEffect(() => rereadAppInfo(client, false), [client]);
  useEffect(() => {
    page.current?.querySelector<HTMLElement>('h1[tabindex="-1"]')?.focus();
  }, []);
  const open = (action: ProtectAction) =>
    showApp((close) => <LockFlow action={action} onClose={close} />);
  return (
    <div className="page" ref={page}>
      <PageHeader title={SECURITY_TITLE} size={22} sub={SECURITY_SUB} />
      <div className="security">
        <StoreScope
          store={info.secrets.store}
          targets={list.data === undefined ? null : storeTargetNames(list.data)}
          error={list.isError ? uiErrorOf(list.error) : null}
        />
        <StoreProtection
          os={info.os}
          mode={info.secrets.mode}
          protectedStore={info.secrets.protected}
          problem={info.secrets.problem}
          info={report.data ?? null}
          error={report.isError ? uiErrorOf(report.error) : null}
          onAction={open}
        />
      </div>
    </div>
  );
}
