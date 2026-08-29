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
