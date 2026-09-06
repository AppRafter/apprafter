// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Layer (a) of the SYS-3 content gate. Validates the fallback JSONs
// (baked into the reproducible landing-web image when
// LANDING_USE_FALLBACK=1) against the shapes the site reads and against
// the phase registry. Catches a broken RELEASED IMAGE before it ships;
// CMS-drift is layer (c)'s (scripts/landing-site-smoke.sh) job.

import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { interestOptions } from './cms/src/collections/waitlistInterestOptions';
import { phaseOptions } from './cms/src/collections/waitlistPhaseOptions';
import { waitlistFields } from './web/src/lib/waitlist-payload';

const ROOT = import.meta.dir;
const FALLBACK = join(ROOT, 'web/src/data/fallback');
const REGISTRY = join(ROOT, 'web/src/data/phases.json');

function readJson(path: string): unknown {
  return JSON.parse(readFileSync(path, 'utf8'));
}

describe('SYS-3 (a) — registry shape', () => {
  test('phases.json exists and every entry has the fixed §3 fields', () => {
    expect(existsSync(REGISTRY)).toBe(true);
    const reg = readJson(REGISTRY) as { phases?: unknown[] };
    expect(Array.isArray(reg.phases)).toBe(true);
    expect(reg.phases?.length).toBeGreaterThan(0);
    const STATUS = new Set(['shipped', 'in-progress', 'planned']);
    for (const raw of reg.phases ?? []) {
      const p = raw as Record<string, unknown>;
      for (const f of ['id', 'label', 'title', 'status', 'anchor']) {
        expect(typeof p[f]).toBe('string');
        expect((p[f] as string).length).toBeGreaterThan(0);
      }
      expect(STATUS.has(p.status as string)).toBe(true);
      expect(p.anchor).toBe(`/#roadmap-phase-${p.id}`);
    }
  });

  test('the five roadmap ids are present', () => {
    const reg = readJson(REGISTRY) as { phases: Array<{ id: string }> };
    const ids = new Set(reg.phases.map((p) => p.id));
    for (const id of ['tier2', 'managed', 'tier3', 'tier4', 'federation']) {
      expect(ids.has(id)).toBe(true);
    }
  });
});

describe('SYS-3 (a) — fallback JSONs are well-formed + present', () => {
  test('every *.json in fallback/ parses as an object', () => {
    const files = readdirSync(FALLBACK).filter((f) => f.endsWith('.json'));
    expect(files.length).toBeGreaterThan(0);
    for (const f of files) {
      const data = readJson(join(FALLBACK, f));
      expect(typeof data).toBe('object');
      expect(data).not.toBeNull();
    }
  });

  test('landingHero fallback carries the CTA + status-badge shape the site reads', () => {
    const hero = readJson(join(FALLBACK, 'landingHero.json')) as Record<string, unknown>;
    const cta = hero.primaryCTA as Record<string, unknown> | undefined;
    expect(typeof cta?.label).toBe('string');
    expect(typeof cta?.href).toBe('string');
    expect(cta?.href as string).not.toMatch(
      /github\.com\/AppRafter\/apprafter\/blob\/[^\s]*\/docs\//,
    );
    expect(typeof hero.statusBadge).toBe('string');
    expect((hero.statusBadge as string).length).toBeGreaterThan(0);
  });
});

describe('SYS-3 (a) — fallback ⊆ registry', () => {
  test('every "Phase N" label in the roadmap fallback exists in the registry', () => {
    const reg = readJson(REGISTRY) as { phases: Array<{ label: string }> };
    const regLabels = new Set(reg.phases.map((p) => p.label));
    const roadmap = readFileSync(join(FALLBACK, 'roadmap.json'), 'utf8');
    const labels = [...roadmap.matchAll(/Phase \d+\+?/g)].map((m) => m[0]);
    expect(labels.length).toBeGreaterThan(0);
    for (const lbl of new Set(labels)) {
      expect(regLabels.has(lbl)).toBe(true);
    }
  });
});

// 2.23c. Two properties of every phase mention in the published copy,
// both of which regressed silently before and would again on the next
// copy edit.
describe('SYS-3 (b) — how a phase is written', () => {
  const FALLBACK_FILES = [
    'advantages.json',
    'comparison.json',
    'landingHero.json',
    'roadmap.json',
    'scalingJourney.json',
    'tierLadder.json',
    'transparency.json',
    'valueProps.json',
    'waitlistCopy.json',
  ];

  test('the "+" suffix is gone from every fallback file', () => {
    // It meant "phase N or later" and came from an internal design
    // brief, where it was explained. On the public site it never was,
    // and it was applied to three of the six phases — so it read as a
    // typo on some and an unexplained tier marker on others.
    for (const f of FALLBACK_FILES) {
      const raw = readFileSync(join(FALLBACK, f), 'utf8');
      expect({ file: f, hit: /Phase \d+(\.\d+)?\+/.test(raw) }).toEqual({
        file: f,
        hit: false,
      });
    }
    const reg = readFileSync(REGISTRY, 'utf8');
    expect(/Phase \d+\+/.test(reg)).toBe(false);
  });

  test('published content carries no internal product numbering', () => {
    // `Product 1` and `Product 2` are this repository's internal names
    // for the two migration products. Both reached the live roadmap and
    // nothing was watching. The feature ledger is about to link to that
    // roadmap, so the leak closes before the link opens.
    let seen = 0;
    for (const f of FALLBACK_FILES) {
      const raw = readFileSync(join(FALLBACK, f), 'utf8');
      seen += 1;
      expect({ file: f, hit: /Product [12]\b/.test(raw) }).toEqual({
        file: f,
        hit: false,
      });
    }
    expect(seen).toBe(FALLBACK_FILES.length);
  });

  test('a phase mention inside CMS HTML is a label, or is on the list of why not', () => {
    // Scoped Astro styles cannot reach `set:html` content, so `.phase-ref`
    // in the global sheet is the ONLY way a CMS-authored mention can look
    // like a phase label. This asserts the *Html fields use it.
    //
    // The allow-list is the record that a mention is known and deferred
    // rather than overlooked — every entry carries its reason.
    const ALLOWED: Record<string, string> = {
      'Phase 1': 'shipped era — no roadmap block exists to anchor to',
      'Phase 2': 'shipped era — no roadmap block exists to anchor to',
      'Phase 3': 'in the roadmap prose itself, where a chip would nest inside its own block',
      'Phase 4': 'in the roadmap prose itself, where a chip would nest inside its own block',
      'Phase 4.5':
        'fractional; the Operations add-on has no roadmap card, so a registry entry ' +
        'would anchor at an id the page never emits — settled under 2.21a, same ' +
        'evidence as Post-launch',
    };
    // RECURSIVE. The first version walked `Object.entries` of the root
    // and passed vacuously: `transparency.json` keeps its `bodyHtml`
    // inside a `cards[]` array, and `comparison.json` inside `rows[]`,
    // so the two files with the most phase references were the two the
    // gate could not see. Verified by deliberately un-labelling one and
    // watching this test stay green — which is why the count assertion
    // below exists as well.
    const htmlFields: Array<[string, string, string]> = [];
    const walk = (file: string, path: string, node: unknown): void => {
      if (typeof node === 'string') {
        if (path.toLowerCase().endsWith('html')) htmlFields.push([file, path, node]);
        return;
      }
      if (Array.isArray(node)) {
        node.forEach((v, i) => walk(file, `${path}[${i}]`, v));
        return;
      }
      if (node && typeof node === 'object') {
        for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
          walk(file, path ? `${path}.${k}` : k, v);
        }
      }
    };
    for (const f of FALLBACK_FILES) {
      walk(f, '', JSON.parse(readFileSync(join(FALLBACK, f), 'utf8')));
    }
    expect(htmlFields.length).toBeGreaterThanOrEqual(5);

    let checked = 0;
    for (const [file, field, value] of htmlFields) {
      for (const m of value.matchAll(/Phase \d+(\.\d+)?/g)) {
        const label = m[0];
        if (ALLOWED[label]) continue;
        checked += 1;
        const around = value.slice(Math.max(0, m.index - 140), m.index + label.length + 20);
        expect({ file, field, label, labelled: around.includes('phase-ref') }).toEqual({
          file,
          field,
          label,
          labelled: true,
        });
      }
    }
    // Non-vacuity: if the walk or the regex stops finding mentions, this
    // test would report success while checking nothing.
    expect(checked).toBeGreaterThanOrEqual(3);
  });

  test('every phase-ref anchor points at a registry anchor, root-relative', () => {
    // Root-relative, not fragment-only: the same copy renders on
    // /privacy and /terms, where a bare `#roadmap-phase-x` goes nowhere.
    const reg = readJson(REGISTRY) as { phases: Array<{ anchor: string }> };
    const anchors = new Set(reg.phases.map((p) => p.anchor));
    let seen = 0;
    for (const f of FALLBACK_FILES) {
      const raw = readFileSync(join(FALLBACK, f), 'utf8');
      for (const m of raw.matchAll(/class=\\?"phase-ref\\?" href=\\?"([^"\\]+)\\?"/g)) {
        seen += 1;
        expect({ file: f, href: m[1], known: anchors.has(m[1]) }).toEqual({
          file: f,
          href: m[1],
          known: true,
        });
      }
    }
    // Non-vacuity: a regex that stops matching would otherwise pass by
    // checking nothing, which is the failure mode this whole file exists
    // to prevent.
    expect(seen).toBeGreaterThanOrEqual(3);
  });
});

// The form posts two independent fields and each has its own select
// options in WaitlistSignups. A value the site can send that the
// collection cannot accept is not a visible failure: Payload answers
// 400 and WaitlistForm reports 400 as already-signed-up, so the
// visitor sees success and no row is written. `federation` shipped in
// the `phases` enum and the roadmap button while never being an
// `interests` option, and lost every signup that clicked it.
describe('SYS-3 (a) — WaitlistSignups accepts everything the site can send', () => {
  test('every phase the roadmap offers a button for is a `phases` option', () => {
    const reg = readJson(REGISTRY) as { phases: Array<{ id: string; status: string }> };
    const accepted = new Set(phaseOptions().map((o) => o.value));
    // Roadmap.astro renders the notify button for non-shipped phases only.
    const offered = reg.phases.filter((p) => p.status !== 'shipped').map((p) => p.id);
    expect(offered.length).toBeGreaterThan(0);
    for (const id of offered) {
      expect(accepted.has(id)).toBe(true);
    }
  });

  test('every interest checkbox the fallback renders is an `interests` option', () => {
    const accepted = new Set(interestOptions.map((o) => o.value));
    const copy = readJson(join(FALLBACK, 'waitlistCopy.json')) as {
      interests: Array<{ key: string }>;
    };
    expect(copy.interests.length).toBeGreaterThan(0);
    for (const it of copy.interests) {
      expect(accepted.has(it.key)).toBe(true);
    }
  });

  test('a phase id is never silently routed into `interests`', () => {
    const reg = readJson(REGISTRY) as { phases: Array<{ id: string }> };
    const offeredKeys = (
      readJson(join(FALLBACK, 'waitlistCopy.json')) as { interests: Array<{ key: string }> }
    ).interests;
    const accepted = new Set(interestOptions.map((o) => o.value));
    for (const p of reg.phases) {
      const fields = waitlistFields({ [p.id]: true }, offeredKeys);
      for (const key of fields.interests ?? []) {
        expect(accepted.has(key)).toBe(true);
      }
      expect(fields.phases).toEqual([p.id]);
    }
  });
});

// The feature ledger and the roadmap were built as two halves of one
// thing and were never joined: no link in either direction, and a Phase
// column of bare text that happened to match a display LABEL. Labels are
// the half that moves — the registry carries a stable `id` precisely so
// a renumber changes what a reader reads and not what they subscribed
// to. This block is the fence that keeps the join on the stable half.
describe('SYS-3 (c) — the feature ledger joins on the registry id', () => {
  const LEDGER = join(ROOT, '../docs/status.md');

  // Every data row's first cell. Separator rows begin `|-`, so `| ` is
  // enough to skip them; the header cell and empty cells are dropped.
  const phaseCells = (): string[] =>
    readFileSync(LEDGER, 'utf8')
      .split('\n')
      .filter((l) => l.startsWith('| '))
      .map((l) => l.split('|')[1].trim())
      .filter((c) => c !== '' && c !== 'Phase');

  test('every phase link resolves to a registry id', () => {
    const reg = readJson(REGISTRY) as { phases: Array<{ id: string }> };
    const ids = new Set(reg.phases.map((p) => p.id));
    const linked = phaseCells().filter((c) => c.startsWith('['));
    // Non-vacuity: a ledger that stopped linking would otherwise pass
    // this by checking nothing.
    expect(linked.length).toBeGreaterThanOrEqual(5);
    for (const cell of linked) {
      const href = cell.match(/\]\(([^)]+)\)/)?.[1];
      expect({ cell, href: href ?? null }).toEqual({ cell, href: href ?? 'MISSING' });
      const id = (href ?? '').split('#')[1]?.replace(/^roadmap-phase-/, '');
      expect({ cell, id, known: ids.has(id ?? '') }).toEqual({ cell, id, known: true });
    }
  });

  test('a phase with a subscribe control is linked, and one without is not', () => {
    // `Roadmap.astro` renders the notify button only where the registry
    // status is not `shipped`, so those labels are exactly the ones with
    // something to point at. `Shipped` has a card but no button, and
    // `Post-launch` is in no registry at all.
    const reg = readJson(REGISTRY) as {
      phases: Array<{ id: string; label: string; status: string }>;
    };
    const subscribable = new Map(
      reg.phases.filter((p) => p.status !== 'shipped').map((p) => [p.label, p.id]),
    );
    let checked = 0;
    for (const cell of phaseCells()) {
      const text = cell.startsWith('[') ? cell.slice(1, cell.indexOf(']')) : cell;
      const want = subscribable.get(text);
      checked += 1;
      expect({ text, linked: cell.startsWith('[') }).toEqual({
        text,
        linked: want !== undefined,
      });
      if (want !== undefined) {
        expect({ text, anchored: cell.includes(`#roadmap-phase-${want}`) }).toEqual({
          text,
          anchored: true,
        });
      }
    }
    expect(checked).toBeGreaterThanOrEqual(20);
  });

  test('the landing links back to the ledger', () => {
    const roadmap = readFileSync(
      join(ROOT, 'web/src/components/sections/Roadmap.astro'),
      'utf8',
    );
    expect(roadmap).toContain('docs.apprafter.dev/status/');
  });
});
