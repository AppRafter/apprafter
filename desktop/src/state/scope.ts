// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The scope a component's screen lives in (ipc/lifecycle.ts): its overlay's, its tab view's, or
// the session's. Provided by the Shell (each tab view, Settings), ViewFrame (each overlay it
// opens) and the app's own overlays; outside every provider, the session's.
import { createContext, useContext } from 'react';
import { type Scope, sessionScope } from '../ipc/lifecycle';

export const ScopeContext = createContext<Scope | null>(null);

export function useScope(): Scope {
  return useContext(ScopeContext) ?? sessionScope();
}
