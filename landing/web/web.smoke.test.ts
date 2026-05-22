// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// landing/web smoke — lock down the Astro scaffold + design-token
// invariants so an accidental delete or drift is caught in CI before
// it lands on apprafter.dev.

import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { cueTokenize } from './src/lib/cue-highlight';

const ROOT = import.meta.dir;

describe('landing/web scaffold', () => {
  test('astro + svelte configs present', () => {
    expect(existsSync(join(ROOT, 'astro.config.ts'))).toBe(true);
    expect(existsSync(join(ROOT, 'svelte.config.js'))).toBe(true);
  });

  test('self-hosted Roboto WOFF2 files committed', () => {
    for (const w of ['400', '500', '700']) {
      expect(existsSync(join(ROOT, `public/fonts/roboto-${w}.woff2`))).toBe(true);
    }
    for (const w of ['400', '500']) {
      expect(existsSync(join(ROOT, `public/fonts/roboto-mono-${w}.woff2`))).toBe(true);
    }
  });

  test('Apache-2.0 attribution for Roboto present', () => {
    const license = readFileSync(join(ROOT, 'public/fonts/LICENSE-Roboto.txt'), 'utf8');
    expect(license).toContain('Apache License');
  });

  test('design tokens include accent + dark/light theme split', () => {
    const tokens = readFileSync(join(ROOT, 'src/styles/tokens.css'), 'utf8');
    expect(tokens).toContain(':root');
    // tokens.css aligns colons — match accent regardless of intervening whitespace.
    expect(tokens).toMatch(/--accent:\s+#14b8a6/);
    expect(tokens).toContain("[data-theme='light']");
    expect(tokens).toMatch(/--bg:\s+#0a0e1a/);
    expect(tokens).toMatch(/--bg:\s+#fafafa/);
  });

  test('theme init script flips data-theme on <html>', () => {
    const init = readFileSync(join(ROOT, 'src/lib/theme-init.ts'), 'utf8');
    expect(init).toContain('THEME_INIT_SCRIPT');
    expect(init).toContain('data-theme');
    expect(init).toContain('apprafter-theme');
  });
});

describe('hero section pieces', () => {
  test('all four hero source files exist', () => {
    for (const f of [
      'src/components/hero/Hero.astro',
      'src/components/hero/HeroCodeBlock.astro',
      'src/components/hero/CodeCopyButton.svelte',
      'src/components/hero/WaitlistForm.svelte',
    ]) {
      expect(existsSync(join(ROOT, f))).toBe(true);
    }
  });

  test('hero copy uses the BRIEF v2.2 headline + license + status badge', () => {
    const hero = readFileSync(join(ROOT, 'src/components/hero/Hero.astro'), 'utf8');
    // Headline — accent wraps "€5 VPS" per BRIEF section 4.2.
    expect(hero).toContain('€5 VPS');
    expect(hero).toContain('Open source');
    // ADR 0032 license string — must NOT slip back to FSL-1.1-MIT.
    expect(hero).toContain('FSL-1.1-Apache-2.0');
    expect(hero).not.toContain('FSL-1.1-MIT');
    // Status badge — Q7 (2026-05-22).
    expect(hero).toContain('v0.3 · Phase 3');
  });

  test('waitlist form POSTs to /api/waitlist-signups (Vite proxy / PUBLIC_CMS_URL)', () => {
    const wl = readFileSync(join(ROOT, 'src/components/hero/WaitlistForm.svelte'), 'utf8');
    expect(wl).toContain('/api/waitlist-signups');
    expect(wl).toContain('PUBLIC_CMS_URL');
    // listens for the toggle event dispatched by Hero.astro
    expect(wl).toContain("'waitlist:toggle'");
  });
});

describe('cue-highlight build-time tokenizer', () => {
  test('keyword + string + number + comment classes attach', () => {
    // The tokenizer matches sections.jsx (renderCue) behaviour:
    // comments are only highlighted when they begin a line; inline
    // trailing // tokens fall back to plain idents. That's fine for
    // the one CUE snippet we ship in hero.
    const html = cueTokenize('apiVersion: "foo"\n// trailing\nreplicas: 3');
    expect(html).toContain('<span class="tok-key">apiVersion</span>');
    expect(html).toContain('<span class="tok-str">&quot;foo&quot;</span>');
    expect(html).toContain('<span class="tok-num">3</span>');
    expect(html).toContain('<span class="tok-cmt">// trailing</span>');
  });

  test('every output line is wrapped in a <div> for stable line height', () => {
    const html = cueTokenize('a: 1\nb: 2\nc: 3');
    const lines = (html.match(/<div/g) ?? []).length;
    expect(lines).toBe(3);
  });

  test('escapes HTML metacharacters in identifiers and strings', () => {
    const html = cueTokenize('x: "<script>"');
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
  });
});
