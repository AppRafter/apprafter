// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Lives at the web root (not under src/) so astro check — which
// type-checks `src` against the strict tsconfig — never sees the
// `bun:test` import.

import { describe, expect, test } from 'bun:test';
import { phaseAnchor } from './src/lib/phase-anchor';

describe('phaseAnchor', () => {
  test('derives the roadmap anchor from a stable phase id', () => {
    expect(phaseAnchor('tier2')).toBe('roadmap-phase-tier2');
    expect(phaseAnchor('federation')).toBe('roadmap-phase-federation');
  });

  test('never doubles "phase-phase" (the R2 regression)', () => {
    for (const id of ['tier1', 'tier2', 'managed', 'tier3', 'tier4', 'federation']) {
      expect(phaseAnchor(id)).not.toContain('phase-phase');
    }
  });

  test('sanitises an id to a safe anchor fragment', () => {
    expect(phaseAnchor('Tier 2')).toBe('roadmap-phase-tier-2');
    expect(phaseAnchor('a/b')).toBe('roadmap-phase-a-b');
  });
});
