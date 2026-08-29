// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Lives at the web root (not under src/) so astro check — which
// type-checks `src` against the strict tsconfig — never sees the
// `bun:test` import.

import { describe, expect, test } from 'bun:test';
import registry from './src/data/phases.json';
import { PHASE_IDS, waitlistFields } from './src/lib/waitlist-payload';

// What waitlistCopy.interests renders today. Kept literal on purpose:
// the point of the split is that the rendered set and the phase
// registry are different sets, so the test must not derive one from
// the other.
const OFFERED = [
  { key: 'tier2' },
  { key: 'observability' },
  { key: 'managed' },
  { key: 'tier3' },
  { key: 'tier4' },
];

describe('waitlistFields', () => {
  test('a preselected phase with no checkbox never becomes an interest', () => {
    // The "Get notified when Phase 8+ ships" roadmap button. Posting
    // `federation` as an interest 400s the whole signup, and the form
    // reports that 400 as success — the signup is lost silently.
    expect(waitlistFields({ federation: true }, OFFERED)).toEqual({
      interests: undefined,
      phases: ['federation'],
    });
  });

  test('a key that is both a checkbox and a phase lands in both fields', () => {
    expect(waitlistFields({ tier2: true }, OFFERED)).toEqual({
      interests: ['tier2'],
      phases: ['tier2'],
    });
  });

  test('a checkbox that is not a phase stays out of phases', () => {
    expect(waitlistFields({ observability: true }, OFFERED)).toEqual({
      interests: ['observability'],
      phases: undefined,
    });
  });

  test('a key that is neither is dropped rather than posted', () => {
    expect(waitlistFields({ 'not-a-thing': true }, OFFERED)).toEqual({
      interests: undefined,
      phases: undefined,
    });
  });

  test('unticked keys are ignored', () => {
    expect(waitlistFields({ tier2: false, federation: false }, OFFERED)).toEqual({
      interests: undefined,
      phases: undefined,
    });
  });

  test('a phase still records when the CMS rendered no interest checkboxes', () => {
    expect(waitlistFields({ managed: true }, null)).toEqual({
      interests: undefined,
      phases: ['managed'],
    });
  });

  test('every roadmap button id the form can preselect reaches `phases`', () => {
    for (const p of registry.phases) {
      expect(waitlistFields({ [p.id]: true }, OFFERED).phases).toEqual([p.id]);
    }
  });

  test('PHASE_IDS is derived from the registry, never restated', () => {
    expect([...PHASE_IDS].sort()).toEqual(registry.phases.map((p) => p.id).sort());
  });
});
