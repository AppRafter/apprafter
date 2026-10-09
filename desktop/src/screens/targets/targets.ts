// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The targets the Targets view lists. The data hook is final; its source is not: `target_list`
// arrives in D.3, so the real app has none yet and says so, and the mock mode hands its
// fixtures in through TargetsSource (main.tsx), which keeps them out of production code.
import { createContext, useContext } from 'react';

/** What a target card shows, typed by hand until D.3 generates it with `target_list`. */
export interface TargetSummary {
  readonly name: string;
  readonly provider: string;
  readonly region: string;
  /** Hardware tier: 1 Solo, 2 Team, 3 Pro, 4 Regulated. */
  readonly tier: 1 | 2 | 3 | 4;
  /** The CLI's default target. */
  readonly cliDefault: boolean;
}

const TIERS = { 1: 'Solo', 2: 'Team', 3: 'Pro', 4: 'Regulated' } as const;

export function tierLabel(tier: TargetSummary['tier']): string {
  return `${TIERS[tier]} (T${tier})`;
}

export const TargetsSource = createContext<readonly TargetSummary[] | null>(null);

export type Targets =
  | { readonly kind: 'unavailable' }
  | { readonly kind: 'ready'; readonly targets: readonly TargetSummary[] };

export function useTargets(): Targets {
  const targets = useContext(TargetsSource);
  return targets === null ? { kind: 'unavailable' } : { kind: 'ready', targets };
}
