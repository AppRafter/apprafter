// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Web-root smoke: PhaseChip reads the registry by id and every id it
// is asked to render must exist in phases.json. The Astro component
// can't run under bun:test, so assert its source contract + registry.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = import.meta.dir;

describe('PhaseChip + registry', () => {
  test('component reads phases.json by id and throws on unknown id', () => {
    const src = readFileSync(join(ROOT, 'src/components/PhaseChip.astro'), 'utf8');
    expect(src).toContain("import phaseRegistry from '../data/phases.json'");
    expect(src).toContain('phaseRegistry.phases.find');
    expect(src).toContain('unknown phase id');
    expect(src).toContain('class="phase-chip"');
  });

  test('registry carries the five stable roadmap ids (+ a shipped labelling entry)', () => {
    const reg = JSON.parse(readFileSync(join(ROOT, 'src/data/phases.json'), 'utf8'));
    const ids = new Set(reg.phases.map((p: { id: string }) => p.id));
    for (const id of ['tier2', 'managed', 'tier3', 'tier4', 'federation']) {
      expect(ids.has(id)).toBe(true);
    }
    for (const p of reg.phases as { id: string; anchor: string; status: string }[]) {
      expect(p.anchor).toBe(`/#roadmap-phase-${p.id}`);
      expect(p.anchor).not.toContain('phase-phase');
      expect(['shipped', 'in-progress', 'planned']).toContain(p.status);
    }
  });
});
