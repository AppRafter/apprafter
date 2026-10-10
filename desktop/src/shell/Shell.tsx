// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Layout variant A (brief §2, §4.3): the title bar with the tabs, then one view per tab and the
// Targets view. Each view sits in an <Activity>: a hidden tab keeps its screen state (section,
// scroll, open dialogs), its effects stop, and its overlays hide with it. Settings is the app's
// own overlay, over every view. The shortcuts listen on the window. While the OS offers no way to
// verify the owner, a notice under the title bar says the app lock is off, on every view. The
// app's own overlays (the add-target wizard, Doctor and what they open: AppOverlays.tsx) render
// over every view, outside the views' Activities; while one is open the tab strip is inert, a tab
// click or close is ignored, and every shortcut but Lock waits, while the title bar's caption
// buttons stay live.
// Each tab view has a scope (ipc/lifecycle.ts) that ends when the tab closes, however it closes,
// and Settings one that ends when it closes: what their screens started goes with them. Never on
// an effect cleanup: a hidden tab's Activity runs those without the tab closing.
import { Activity, useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { WarningCircleIcon } from '../components/icons';
import { ToastViewport, useToast } from '../components/Toast';
import { newScope, type ScopeHandle, sessionScope } from '../ipc/lifecycle';
import { useListRefresh } from '../ipc/listRefresh';
import { refreshList, useOperations } from '../ipc/operations';
import { PlannedSection } from '../screens/placeholders/PlannedSection';
import { TargetScreen } from '../screens/target/TargetScreen';
import { TargetsPage } from '../screens/targets/TargetsPage';
import { lockOff, lockOffMessage, NO_AUTH_NOTICE, useLockActions } from '../state/lock';
import { usePlatform } from '../state/platform';
import { ScopeContext } from '../state/scope';
import { closedTabs, INITIAL_SESSION, sessionReducer, type TargetTab } from '../state/session';
import { useSettings } from '../state/settings';
import { shortcutFor } from '../state/shortcuts';
import { TabContext } from '../state/tab';
import { AppOverlayContext, useAppOverlayHost } from './AppOverlays';
import { ClusterMeta } from './ClusterMeta';
import { EndedAwayNotices } from './EndedAway';
import { ScreenShown } from './reveal';
import { SettingsDialog } from './SettingsDialog';
import { Sidebar } from './Sidebar';
import { TabStrip, tabId, tabPanelId } from './TabStrip';
import { TitleBar } from './TitleBar';
import { ViewFrame } from './ViewFrame';

const report = (what: string) => (error: unknown) => {
  console.error(`${what} failed:`, error);
};

export function Shell() {
  const info = usePlatform();
  const { os } = info;
  const [session, dispatch] = useReducer(sessionReducer, INITIAL_SESSION);
  const app = useAppOverlayHost();
  const operations = useOperations();
  const running = useMemo(
    () =>
      new Set(
        [...operations.values()].flatMap((op) =>
          op.summary?.state === 'running' && op.summary.target !== null ? [op.summary.target] : [],
        ),
      ),
    [operations],
  );

  // The targets with a tab: their cards on the Targets view switch to it.
  const openTargets = useMemo(() => new Set(session.tabs.map((tab) => tab.target)), [session.tabs]);

  // Settings' scope while it is open: its close ends it.
  const [settings, setSettings] = useState<ScopeHandle | null>(null);
  const settingsRef = useRef<ScopeHandle | null>(null);
  const { lock: lockNow } = useLockActions();
  const toast = useToast();
  // Not in effect, the lock is not offered: Lock is disabled and Mod+L says why instead.
  const off = lockOff(info.auth.available, useSettings().data?.lockEnabled);
  const showTargets = useCallback(() => dispatch({ type: 'show', view: { kind: 'targets' } }), []);
  const openSettings = useCallback(() => {
    if (settingsRef.current !== null) return;
    const handle = newScope(sessionScope());
    settingsRef.current = handle;
    setSettings(handle);
  }, []);
  const closeSettings = useCallback(() => {
    settingsRef.current?.end();
    settingsRef.current = null;
    setSettings(null);
  }, []);
  const lock = useCallback(() => {
    if (off !== null) {
      toast({ message: lockOffMessage(off), icon: WarningCircleIcon });
      return;
    }
    lockNow().catch(report('lock_now'));
  }, [off, toast, lockNow]);

  useEffect(() => {
    refreshList().catch(report('op_list'));
  }, []);

  // Each tab view's scope, made the first time the tab renders.
  const tabScopes = useRef(new Map<string, ScopeHandle>());
  const scopeOf = (key: string) => {
    let handle = tabScopes.current.get(key);
    if (handle === undefined) {
      handle = newScope(sessionScope());
      tabScopes.current.set(key, handle);
    }
    return handle.scope;
  };
  // A tab that closed, however (its close button, a remove, a rename onto another tab): its
  // scope ends, and what its screens started goes — the reads cancelled, the plans its confirms
  // hold discarded in Rust with what they hold. Here, in the Shell, because a hidden tab's
  // Activity runs its own effect cleanups without closing.
  const lastTabs = useRef(session.tabs);
  useEffect(() => {
    for (const key of closedTabs(lastTabs.current, session.tabs)) {
      tabScopes.current.get(key)?.end();
      tabScopes.current.delete(key);
    }
    lastTabs.current = session.tabs;
  }, [session.tabs]);
  useListRefresh();

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const action = shortcutFor(event, os);
      if (action === null) return;
      event.preventDefault();
      // An app overlay is open: the window waits behind it, the lock alone does not.
      if (app.open && action !== 'lock') return;
      if (action === 'targets') showTargets();
      else if (action === 'settings') openSettings();
      else lock();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [os, showTargets, openSettings, lock, app.open]);

  const sidebar = (tab: TargetTab | null) => (
    <Sidebar
      os={os}
      tab={tab}
      info={info}
      onNavigate={(section) => {
        if (tab !== null) dispatch({ type: 'navigate', key: tab.key, section });
      }}
      onShowTargets={showTargets}
      onLock={lock}
      lockDisabled={off !== null}
      onSettings={openSettings}
      {...(tab !== null && { meta: <ClusterMeta target={tab.target} /> })}
    />
  );

  return (
    <AppOverlayContext value={app.show}>
      <div className="app">
        <TitleBar os={os}>
          <TabStrip
            tabs={session.tabs}
            view={session.view}
            os={os}
            running={running}
            inert={app.open}
            onShow={(view) => {
              if (!app.open) dispatch({ type: 'show', view });
            }}
            onClose={(key) => {
              if (!app.open) dispatch({ type: 'closeTab', key });
            }}
            onNewTab={() => {
              if (!app.open) showTargets();
            }}
          />
        </TitleBar>
        {off === 'no_auth' && (
          <p className="shell-notice" role="note">
            {NO_AUTH_NOTICE}
          </p>
        )}
        <div className="shell-body">
          {session.tabs.map((tab) => {
            const shown = session.view.kind === 'tab' && session.view.key === tab.key;
            return (
              <Activity key={tab.key} mode={shown ? 'visible' : 'hidden'}>
                <ScopeContext value={scopeOf(tab.key)}>
                  <TabContext value={{ tab, active: shown }}>
                    <ViewFrame panel={{ id: tabPanelId(tab.key), labelledBy: tabId(tab.key) }}>
                      {sidebar(tab)}
                      <main className="main">
                        {tab.section === 'target' ? (
                          <TargetScreen
                            name={tab.target}
                            onRenamed={(from, to) => dispatch({ type: 'targetRenamed', from, to })}
                            onRemoved={(target) => dispatch({ type: 'targetRemoved', target })}
                          />
                        ) : (
                          <PlannedSection section={tab.section} target={tab.target} />
                        )}
                      </main>
                    </ViewFrame>
                  </TabContext>
                </ScopeContext>
              </Activity>
            );
          })}
          <Activity mode={session.view.kind === 'targets' ? 'visible' : 'hidden'}>
            <ViewFrame>
              {sidebar(null)}
              <main className="main">
                <TargetsPage
                  openTargets={openTargets}
                  onOpen={(target) =>
                    dispatch({ type: 'openTarget', target, key: crypto.randomUUID() })
                  }
                  onRemoved={(target) => dispatch({ type: 'targetRemoved', target })}
                />
              </main>
            </ViewFrame>
          </Activity>
          {settings !== null && (
            <ScopeContext value={settings.scope}>
              <SettingsDialog onClose={closeSettings} />
            </ScopeContext>
          )}
          {app.overlays}
          <EndedAwayNotices />
          <ToastViewport />
        </div>
        <ScreenShown />
      </div>
    </AppOverlayContext>
  );
}
