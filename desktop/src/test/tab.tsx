// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A screen in a tab view as the Shell hosts it, for bun tests: the tab's scope (ended when the tab
// closes), the <Activity> that hides it while another tab shows, its TabContext and its
// ViewFrame. hide() and show() switch the Activity as a tab switch does; close() removes the tab
// and ends its scope, as the Shell does (ipc/lifecycle.ts).
import { act } from '@testing-library/react';
import { Activity, type ReactNode, useState } from 'react';
import { newScope, type Scope, sessionScope } from '../ipc/lifecycle';
import { ViewFrame } from '../shell/ViewFrame';
import { ScopeContext } from '../state/scope';
import { TabContext } from '../state/tab';

export interface TabHost {
  /** The tab: wrap the screen in it. */
  readonly Tab: (props: { children: ReactNode }) => ReactNode;
  readonly scope: Scope;
  readonly hide: () => void;
  readonly show: () => void;
  readonly close: () => void;
}

export function tabHost(target = 'staging', key = 'tab-1'): TabHost {
  const handle = newScope(sessionScope());
  let set: ((next: { shown: boolean; open: boolean }) => void) | null = null;
  function Tab({ children }: { children: ReactNode }) {
    const [state, setState] = useState({ shown: true, open: true });
    set = setState;
    if (!state.open) return null;
    return (
      <Activity mode={state.shown ? 'visible' : 'hidden'}>
        <ScopeContext value={handle.scope}>
          <TabContext value={{ tab: { key, target, section: 'target' }, active: state.shown }}>
            <ViewFrame>{children}</ViewFrame>
          </TabContext>
        </ScopeContext>
      </Activity>
    );
  }
  const to = (next: { shown: boolean; open: boolean }) => {
    act(() => set?.(next));
  };
  return {
    Tab,
    scope: handle.scope,
    hide: () => to({ shown: false, open: true }),
    show: () => to({ shown: true, open: true }),
    close: () => {
      to({ shown: false, open: false });
      handle.end();
    },
  };
}
