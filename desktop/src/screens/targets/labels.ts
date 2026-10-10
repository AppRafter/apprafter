// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// How a target's stored fields read: its default tier, its provider, the sidebar's meta line.

import type { TargetSummary } from '../../ipc/generated/TargetSummary';
import type { SUPPORTED_PROVIDERS, TIERS } from '../../ipc/generated/target';

type TierLevel = (typeof TIERS)[number]['level'];

/** The hardware tiers by level (spec §2); a level the core adds is a type error here. */
const TIER_NAMES: Record<TierLevel, string> = { 1: 'Solo', 2: 'Team', 3: 'Pro', 4: 'Regulated' };

const PROVIDER_NAMES: Record<(typeof SUPPORTED_PROVIDERS)[number], string> = {
  'hetzner-cloud': 'Hetzner Cloud',
};

const tierName = (level: number | null): string | null =>
  level !== null && Object.hasOwn(TIER_NAMES, level) ? TIER_NAMES[level as TierLevel] : null;

/**
 * The default tier the CLI stores, as a hint: `Team (T2)` for a known tier, the raw string for
 * one the core does not parse, `not set` for none.
 */
export function tierLabel(defaultTier: string | null, tierLevel: number | null): string {
  const name = tierName(tierLevel);
  if (name !== null) return `${name} (T${tierLevel})`;
  if (defaultTier !== null) return `${defaultTier} (not a known tier)`;
  return 'not set';
}

/** `T2` for a known tier, the raw string for another, null for none. */
export function tierShort(defaultTier: string | null, tierLevel: number | null): string | null {
  return tierName(tierLevel) !== null ? `T${tierLevel}` : defaultTier;
}

/** `Hetzner Cloud` for `hetzner-cloud`; a provider id this app does not know shows as it is. */
export function providerLabel(provider: string): string {
  return Object.hasOwn(PROVIDER_NAMES, provider)
    ? PROVIDER_NAMES[provider as keyof typeof PROVIDER_NAMES]
    : provider;
}

/** The sidebar's line under a target: provider, region and tier, whichever are set. */
export function metaLine(
  summary: Pick<TargetSummary, 'provider' | 'region' | 'defaultTier' | 'tierLevel'>,
): string {
  return [summary.provider, summary.region, tierShort(summary.defaultTier, summary.tierLevel)]
    .filter((part) => part !== null)
    .join(' · ');
}

export function serverLine(serverType: string | null): string {
  return `server type ${serverType ?? 'not set'}`;
}
