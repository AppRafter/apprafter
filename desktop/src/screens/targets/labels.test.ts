// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { TIERS } from '../../ipc/generated/target';
import { targetSummary } from '../../test/fixtures';
import { metaLine, providerLabel, serverLine, tierLabel, tierShort } from './labels';

test('a tier reads as the hint the CLI stores: name and hardware tier, raw, or not set', () => {
  expect(tierLabel('team', 2)).toBe('Team (T2)');
  expect(tierLabel('prod', 3)).toBe('Pro (T3)');
  expect(tierLabel('gold', null)).toBe('gold (not a known tier)');
  expect(tierLabel(null, null)).toBe('not set');
  expect(TIERS.map(({ id, level }) => tierLabel(id, level))).toEqual([
    'Solo (T1)',
    'Team (T2)',
    'Pro (T3)',
    'Regulated (T4)',
  ]);
  for (const { level } of TIERS) expect(tierLabel('x', level)).toMatch(/^\w+ \(T\d\)$/);
});

test('the sidebar meta line: provider, region, tier — what is set', () => {
  expect(metaLine(targetSummary())).toBe('hetzner-cloud · nbg1 · T2');
  expect(metaLine(targetSummary({ region: null, tierLevel: null, defaultTier: null }))).toBe(
    'hetzner-cloud',
  );
  expect(metaLine(targetSummary({ tierLevel: null, defaultTier: 'gold' }))).toBe(
    'hetzner-cloud · nbg1 · gold',
  );
  expect(tierShort('team', 2)).toBe('T2');
  expect(tierShort('gold', null)).toBe('gold');
  expect(tierShort(null, null)).toBeNull();
  expect(providerLabel('hetzner-cloud')).toBe('Hetzner Cloud');
  expect(providerLabel('other')).toBe('other');
  expect(providerLabel('toString')).toBe('toString');
  expect(serverLine('cx22')).toBe('server type cx22');
  expect(serverLine(null)).toBe('server type not set');
});
