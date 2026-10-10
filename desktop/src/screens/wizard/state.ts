// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The add-target wizard as a pure reducer. The token is in `token` only until it is verified;
// from then on the wizard holds the draft id that stands for it in Rust (decision 6), and the
// field is empty. A lost draft (expired, locked, or taken by a plan whose run failed) sends the
// owner back to the provider step with every other choice kept.
import type { DraftId } from '../../ipc/generated/DraftId';
import type { MachineCatalogue } from '../../ipc/generated/MachineCatalogue';
import type { TargetAddArgs } from '../../ipc/generated/TargetAddArgs';
import type { TargetAdded } from '../../ipc/generated/TargetAdded';
import { SUPPORTED_PROVIDERS, TIERS } from '../../ipc/generated/target';
import { choosable, defaultRegion, defaultSku, offerIn } from '../machine/catalogue';

export type Step = 0 | 1 | 2;
export type SshChoice =
  | { readonly kind: 'key'; readonly path: string }
  | { readonly kind: 'other'; readonly text: string; readonly checked: string | null }
  | { readonly kind: 'skip' };

export interface WizardState {
  readonly step: Step;
  readonly provider: string;
  readonly token: string;
  readonly draft: DraftId | null;
  readonly catalogue: MachineCatalogue | null;
  readonly region: string | null;
  readonly sku: string | null;
  readonly name: string;
  readonly tier: string | null;
  readonly ssh: SshChoice | null;
}

export type WizardAction =
  | { readonly type: 'provider'; readonly value: string }
  | { readonly type: 'token'; readonly value: string }
  | { readonly type: 'verified'; readonly draft: DraftId }
  | { readonly type: 'forgetDraft' }
  | { readonly type: 'draftTaken' }
  | { readonly type: 'catalogue'; readonly catalogue: MachineCatalogue }
  | { readonly type: 'region'; readonly value: string }
  | { readonly type: 'sku'; readonly value: string }
  | { readonly type: 'name'; readonly value: string }
  | { readonly type: 'tier'; readonly value: string }
  | { readonly type: 'ssh'; readonly value: SshChoice }
  | { readonly type: 'go'; readonly step: Step };

export function initialWizard(): WizardState {
  return {
    step: 0,
    provider: SUPPORTED_PROVIDERS[0] ?? '',
    token: '',
    draft: null,
    catalogue: null,
    region: null,
    sku: null,
    name: '',
    tier: TIERS[0]?.id ?? null,
    ssh: null,
  };
}

/** The chosen SKU when `region` offers it choosably, else that region's recommended one, or none. */
function keepSku(cat: MachineCatalogue, region: string, sku: string | null): string | null {
  return choosable(offerIn(cat, region, sku)) ? sku : defaultSku(cat, region);
}

export function wizardReducer(s: WizardState, a: WizardAction): WizardState {
  switch (a.type) {
    case 'provider':
      return a.value === s.provider ? s : { ...s, provider: a.value, draft: null, catalogue: null };
    case 'token':
      return { ...s, token: a.value };
    case 'verified':
      return { ...s, token: '', draft: a.draft, catalogue: null };
    case 'forgetDraft':
      return { ...s, draft: null, catalogue: null, step: 0 };
    case 'draftTaken':
      return { ...s, draft: null };
    case 'catalogue': {
      const offered = new Set(a.catalogue.offers.map((o) => o.location));
      const region =
        s.region !== null && offered.has(s.region) ? s.region : defaultRegion(a.catalogue);
      return {
        ...s,
        catalogue: a.catalogue,
        region,
        sku: region === null ? null : keepSku(a.catalogue, region, s.sku),
      };
    }
    case 'region':
      return {
        ...s,
        region: a.value,
        sku: s.catalogue === null ? null : keepSku(s.catalogue, a.value, s.sku),
      };
    case 'sku':
      return { ...s, sku: a.value };
    case 'name':
      return { ...s, name: a.value };
    case 'tier':
      return { ...s, tier: a.value };
    case 'ssh':
      return { ...s, ssh: a.value };
    case 'go':
      return { ...s, step: a.step };
  }
}

/** What op_plan_target_add takes; never the token (the draft stands for it). */
export function addArgs(s: WizardState, sshKey: string | null): TargetAddArgs | null {
  if (s.draft === null || s.region === null || s.sku === null || s.tier === null) return null;
  return {
    name: s.name,
    provider: s.provider,
    draftId: s.draft,
    sshKey,
    region: s.region,
    tier: s.tier,
    serverType: s.sku,
  };
}

export function addedMessage(added: TargetAdded): string {
  const saved = `Target “${added.name}” saved.`;
  return added.cliDefault === null
    ? saved
    : `${saved} Your CLI default is now ${added.cliDefault.to ?? added.name}.`;
}
