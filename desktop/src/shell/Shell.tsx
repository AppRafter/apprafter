// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Layout variant A (brief §2, §4.3): the title bar with the tabs, then one view per tab and the
// Targets view. Each view sits in an <Activity>: a hidden tab keeps its screen state (section,
// scroll, open dialogs), its effects stop, and its overlays hide with it. Settings is the app's
// own overlay, over every view. The shortcuts listen on the window.
import { Activity, useCallback, useEffect, useMemo, useReducer, useState } from 'react';
import { ToastViewport } from '../components/Toast';
import { refreshList, useOperations } from '../ipc/operations';
import { PlannedSection } from '../screens/placeholders/PlannedSection';
import { TargetsPage } from '../screens/targets/TargetsPage';
import { useLockActions } from '../state/lock';
import { usePlatform } from '../state/platform';
import { INITIAL_SESSION, sessionReducer, type TargetTab } from '../state/session';
import { shortcutFor } from '../state/shortcuts';
import { TabContext } from '../state/tab';
import { SettingsDialog } from './SettingsDialog';
import { Sidebar } from './Sidebar';
import { TabStrip } from './TabStrip';
import { TitleBar } from './TitleBar';
import { ViewFrame } from './ViewFrame';

const report = (what: string) => (error: unknown) => {
  console.error(`${what} failed:`, error);
};

export function Shell() {
  const info = usePlatform();
  const { os } = info;
  const [session, dispatch] = useReducer(sessionReducer, INITIAL_SESSION);
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

  const [settingsOpen, setSettingsOpen] = useState(false);
  const { lock: lockNow } = useLockActions();
  const showTargets = useCallback(() => dispatch({ type: 'show', view: { kind: 'targets' } }), []);
  const openSettings = useCallback(() => setSettingsOpen(true), []);
  const lock = useCallback(() => {
    lockNow().catch(report('lock_now'));
  }, [lockNow]);

  useEffect(() => {
    refreshList().catch(report('op_list'));
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const action = shortcutFor(event, os);
      if (action === null) return;
      event.preventDefault();
      if (action === 'targets') showTargets();
      else if (action === 'settings') openSettings();
      else lock();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [os, showTargets, openSettings, lock]);

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
      onSettings={openSettings}
    />
  );

  return (
    <div className="app">
      <TitleBar os={os}>
        <TabStrip
          tabs={session.tabs}
          view={session.view}
          os={os}
          running={running}
          onShow={(view) => dispatch({ type: 'show', view })}
          onClose={(key) => dispatch({ type: 'closeTab', key })}
          onNewTab={showTargets}
        />
      </TitleBar>
      <div className="shell-body">
        {session.tabs.map((tab) => {
          const shown = session.view.kind === 'tab' && session.view.key === tab.key;
          return (
            <Activity key={tab.key} mode={shown ? 'visible' : 'hidden'}>
              <TabContext value={{ tab, active: shown }}>
                <ViewFrame>
                  {sidebar(tab)}
                  <main className="main">
                    <PlannedSection section={tab.section} target={tab.target} />
                  </main>
                </ViewFrame>
              </TabContext>
            </Activity>
          );
        })}
        <Activity mode={session.view.kind === 'targets' ? 'visible' : 'hidden'}>
          <ViewFrame>
            {sidebar(null)}
            <main className="main">
              <TargetsPage
                onOpen={(target) =>
                  dispatch({ type: 'openTarget', target, key: crypto.randomUUID() })
                }
              />
            </main>
          </ViewFrame>
        </Activity>
        {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
        <ToastViewport />
      </div>
    </div>
  );
}
