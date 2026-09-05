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

  test('the chip box model is global, and shared with the CMS-reachable class', () => {
    // Astro scopes a component <style> with a data-astro-cid-* attribute
    // selector, and content injected through set:html never carries that
    // attribute — so a chip class a CMS field is allowed to use CANNOT
    // live in the component. One rule, two selectors, in the global sheet.
    const css = readFileSync(join(ROOT, 'src/styles/global.css'), 'utf8');
    const rule = css.match(/\.phase-chip,\s*\n\.phase-ref\s*\{[^}]*\}/)?.[0];
    expect(rule).toBeDefined();
    // The declaration that fixes the stretch: the chip is a grid item in
    // BoringTech and an `auto` width fills the whole 200px track. Read
    // out of the RULE, not out of the whole sheet — `width: fit-content`
    // elsewhere in global.css would satisfy a file-wide check while the
    // chip still stretched.
    expect(rule).toContain('width: fit-content');
    expect(css).toMatch(/\.phase-chip:hover,\s*\n\.phase-ref:hover/);
  });

  test('the component no longer scopes the rule, so the shared half cannot be lost', () => {
    const src = readFileSync(join(ROOT, 'src/components/PhaseChip.astro'), 'utf8');
    expect(src).not.toContain('<style>');
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
