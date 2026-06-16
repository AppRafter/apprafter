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
    // Phase H moved hero copy from Hero.astro into the
    // landingHero.json fallback (mirroring the Payload global).
    const heroJson = readFileSync(join(ROOT, 'src/data/fallback/landingHero.json'), 'utf8');
    expect(heroJson).toContain('€5 VPS');
    expect(heroJson).toContain('Open source');
    // ADR 0032 license string — must NOT slip back to FSL-1.1-MIT.
    expect(heroJson).toContain('FSL-1.1-Apache-2.0');
    expect(heroJson).not.toContain('FSL-1.1-MIT');
    // Status badge — honesty pass (2026-06-15): Phase 2 shipped, Tier 1 live,
    // Tier 2 + managed still in development (no "shipped on Tier 2" claim).
    expect(heroJson).toContain('v0.2 · Phase 2 shipped');
    expect(heroJson).not.toContain('shipped on Tier 1 and Tier 2');
    // Hero CUE manifest is valid against schemas/v1alpha1 (see Task 1):
    // correct env forms + public hostname, none of the old invalid syntax.
    expect(heroJson).toContain('claim.pg.url');
    expect(heroJson).toContain('secret: \\"stripe/api-key\\"');
    expect(heroJson).toContain('hostname: \\"billing.example.com\\"');
    expect(heroJson).not.toContain('public:  true');
    expect(heroJson).not.toContain('claim.pg.uri');
    expect(heroJson).not.toContain('network: \\"vpn\\"');
  });

  test('waitlist form POSTs to /api/waitlist-signups (Vite proxy / PUBLIC_CMS_URL)', () => {
    const wl = readFileSync(join(ROOT, 'src/components/hero/WaitlistForm.svelte'), 'utf8');
    expect(wl).toContain('/api/waitlist-signups');
    expect(wl).toContain('PUBLIC_CMS_URL');
    // listens for the toggle event dispatched by Hero.astro
    expect(wl).toContain("'waitlist:toggle'");
  });
});

describe('page shell (Phase G)', () => {
  test('BaseLayout owns <head>, og:*, twitter:*, JSON-LD slot', () => {
    const layoutPath = join(ROOT, 'src/components/layout/BaseLayout.astro');
    expect(existsSync(layoutPath)).toBe(true);
    const layout = readFileSync(layoutPath, 'utf8');
    // Required <head> surface.
    expect(layout).toContain('<title>');
    expect(layout).toContain('og:site_name');
    expect(layout).toContain('og:image');
    expect(layout).toContain('twitter:card');
    expect(layout).toContain('rel="canonical"');
    expect(layout).toContain('rel="preload"');
    // Allows page-specific head injection (JSON-LD).
    expect(layout).toContain('<slot name="head" />');
  });

  test('index.astro emits Organization + SoftwareApplication JSON-LD', () => {
    const idx = readFileSync(join(ROOT, 'src/pages/index.astro'), 'utf8');
    // Quote style follows Biome's single-quote formatting; assert
    // the type values rather than the exact quoting.
    expect(idx).toMatch(/'@type':\s*'Organization'/);
    expect(idx).toMatch(/'@type':\s*'SoftwareApplication'/);
    expect(idx).toContain('application/ld+json');
  });
});

describe('favicon — well-formed, theme-adaptive, cache-busted', () => {
  const svg = readFileSync(join(ROOT, 'public/favicon.svg'), 'utf8');

  test('no double-hyphen inside any XML comment (regression)', () => {
    // `--` inside <!-- --> makes the SVG not well-formed XML; the browser
    // refuses to parse it and the favicon silently breaks.
    const commentBodies = [...svg.matchAll(/<!--([\s\S]*?)-->/g)].map((m) => m[1]);
    expect(commentBodies.length).toBeGreaterThan(0);
    for (const body of commentBodies) {
      expect(body).not.toContain('--');
    }
  });

  test('transparent + theme-adaptive + on-palette', () => {
    expect(svg).toContain('prefers-color-scheme'); // mark follows OS theme
    expect(svg).toContain('#14b8a6'); // brand accent kept on the roof
    expect(svg).not.toContain('#03110e'); // off-palette foreground gone
    expect(svg).not.toContain('<rect'); // no filled tile — transparent
  });

  test('a legacy favicon.ico is shipped for bare-root browser probes', () => {
    expect(existsSync(join(ROOT, 'public/favicon.ico'))).toBe(true);
  });

  test('BaseLayout cache-busts every icon + og ref through assetVersion', () => {
    const layout = readFileSync(join(ROOT, 'src/components/layout/BaseLayout.astro'), 'utf8');
    expect(layout).toContain('assetVersion');
    // each icon href carries a content-hash ?v= query
    expect(layout).toMatch(/\/favicon\.svg\?v=/);
    expect(layout).toMatch(/assetVersion\('favicon\.ico'\)/);
    expect(layout).toMatch(/assetVersion\('favicon-16\.png'\)/);
    expect(layout).toMatch(/assetVersion\('apple-touch-icon\.png'\)/);
    // og:image is versioned too (local assets only)
    expect(layout).toContain('ogImageVersioned');
  });
});

describe('polish — sitemap, robots, 404, legal stubs (Phase I)', () => {
  test('robots.txt allows everything + advertises sitemap', () => {
    const r = readFileSync(join(ROOT, 'public/robots.txt'), 'utf8');
    expect(r).toContain('User-agent: *');
    expect(r).toContain('Allow: /');
    expect(r).toContain('Sitemap: https://apprafter.dev/sitemap-index.xml');
  });

  test('Astro sitemap integration registered in astro.config.ts', () => {
    const cfg = readFileSync(join(ROOT, 'astro.config.ts'), 'utf8');
    expect(cfg).toContain('@astrojs/sitemap');
    expect(cfg).toContain('sitemap()');
  });

  test('404 + privacy + terms pages present + carry noindex robots meta', () => {
    for (const page of ['404.astro', 'privacy.astro', 'terms.astro']) {
      const p = join(ROOT, 'src/pages', page);
      expect(existsSync(p)).toBe(true);
      const src = readFileSync(p, 'utf8');
      expect(src).toContain('noindex');
    }
  });

  test('Privacy + Terms reference ADR 0032 / FSL-1.1-Apache-2.0 instead of MIT', () => {
    for (const page of ['privacy.astro', 'terms.astro']) {
      const src = readFileSync(join(ROOT, 'src/pages', page), 'utf8');
      expect(src).not.toContain('FSL-1.1-MIT');
    }
    const terms = readFileSync(join(ROOT, 'src/pages/terms.astro'), 'utf8');
    expect(terms).toContain('FSL-1.1-Apache-2.0');
  });
});

describe('content sections (Phase F)', () => {
  test('all eight section source files exist', () => {
    for (const f of [
      'src/components/sections/ValueProps.astro',
      'src/components/sections/ScalingJourney.astro',
      'src/components/sections/TierLadder.astro',
      'src/components/sections/Comparison.astro',
      'src/components/sections/BoringTech.astro',
      'src/components/sections/Advantages.astro',
      'src/components/sections/Roadmap.astro',
      'src/components/sections/BootstrapStrip.astro',
    ]) {
      expect(existsSync(join(ROOT, f))).toBe(true);
    }
  });

  test('roadmap phase ids are stable anchors for comparison cross-links', () => {
    // The slug formula lives in Roadmap.astro and the link target
    // lives in comparison.json — both must agree on the same id.
    const rm = readFileSync(join(ROOT, 'src/components/sections/Roadmap.astro'), 'utf8');
    const cmpJson = readFileSync(join(ROOT, 'src/data/fallback/comparison.json'), 'utf8');
    const roadmapJson = readFileSync(join(ROOT, 'src/data/fallback/roadmap.json'), 'utf8');
    // Slug-builder function still in component.
    expect(rm).toContain('roadmap-phase-');
    // Comparison row links to #roadmap-phase-phase-8.
    expect(cmpJson).toContain('#roadmap-phase-phase-8');
    // Roadmap data carries the matching Phase 8+ phase.
    expect(roadmapJson).toContain('Phase 8+');
  });

  test('all section eyebrows are unique (no copy-paste collisions)', () => {
    // Eyebrows moved out of Hero.astro into the fallback JSONs.
    // Read each section's JSON and assert the set has the right
    // size — no duplicates.
    const sectionJsons = [
      'valueProps', // ValueProps uses a hardcoded "Why AppRafter"
      'scalingJourney',
      'tierLadder',
      'comparison',
      'boringTech',
      'advantages',
      'roadmap',
    ];
    const eyebrows = new Set<string>();
    // ValueProps eyebrow lives in the component (no eyebrow field
    // in the global) — pick it up from the source.
    const vp = readFileSync(join(ROOT, 'src/components/sections/ValueProps.astro'), 'utf8');
    const vpEyebrow = vp.match(/eyebrow="([^"]+)"/)?.[1];
    expect(vpEyebrow).toBeTruthy();
    if (vpEyebrow) eyebrows.add(vpEyebrow);

    for (const name of sectionJsons.slice(1)) {
      const data = JSON.parse(readFileSync(join(ROOT, `src/data/fallback/${name}.json`), 'utf8'));
      expect(data.eyebrow).toBeTruthy();
      expect(eyebrows.has(data.eyebrow)).toBe(false);
      eyebrows.add(data.eyebrow);
    }
    // ValueProps + 6 JSON eyebrows = 7 unique.
    expect(eyebrows.size).toBe(7);
  });
});

describe('CMS client (Phase H)', () => {
  test('lib/cms.ts exposes one getter per Payload global', () => {
    const cms = readFileSync(join(ROOT, 'src/lib/cms.ts'), 'utf8');
    const getters = [
      'getSiteSettings',
      'getLandingHero',
      'getValueProps',
      'getScalingJourney',
      'getTierLadder',
      'getComparison',
      'getTransparency',
      'getBoringTech',
      'getAdvantages',
      'getRoadmap',
      'getBootstrapStrip',
      'getFooterContent',
      'getWaitlistCopy',
    ];
    for (const g of getters) {
      expect(cms).toContain(`export const ${g}`);
    }
    // Dev fallback path + prod fail-loudly path both wired.
    expect(cms).toContain('import.meta.glob');
    expect(cms).toContain('PROD');
    expect(cms).toContain('DEV');
  });

  test('fallback JSON for each global is present', () => {
    const files = [
      'siteSettings',
      'landingHero',
      'valueProps',
      'scalingJourney',
      'tierLadder',
      'comparison',
      'transparency',
      'boringTech',
      'advantages',
      'roadmap',
      'bootstrapStrip',
      'footer',
      'waitlistCopy',
    ];
    for (const f of files) {
      expect(existsSync(join(ROOT, `src/data/fallback/${f}.json`))).toBe(true);
    }
  });

  test('renderCopyright {{year}} substitution helper is exported from cms.ts', () => {
    // The runtime function relies on Vite's import.meta.glob and
    // can't be loaded under Bun test directly — assert its source
    // shape instead.
    const cms = readFileSync(join(ROOT, 'src/lib/cms.ts'), 'utf8');
    expect(cms).toMatch(/export function renderCopyright\(/);
    expect(cms).toContain('{{year}}');
  });

  test('LANDING_USE_FALLBACK=1 opts the prod build into JSON fallbacks', () => {
    // Docker build sets this so the image is reproducible without
    // a reachable Payload at image-build time.
    const cms = readFileSync(join(ROOT, 'src/lib/cms.ts'), 'utf8');
    expect(cms).toContain('LANDING_USE_FALLBACK');
    expect(cms).toContain('USE_FALLBACK');
  });
});

describe('Application manifests — prod + preview pair', () => {
  // Walk-fix 2026-05-25: temporarily pinned to :latest while
  // the landing-promote-to-prod.yml + landing-preview-build.yml
  // workflows haven't yet seeded :prod / :preview tags in
  // ghcr. The registry currently carries only :latest +
  // landing-vX.Y.Z, so pinning to :prod / :preview produces
  // ImagePullBackOff on real clusters. Revisit once the
  // promotion workflows fire and the manifests can flip back
  // to their proper rolling-tag conventions.
  test('Application.cue (prod) pins landing-web image + carries apprafter.dev hostname label', () => {
    const m = readFileSync(join(ROOT, 'apprafter/Application.cue'), 'utf8');
    expect(m).toContain('name:      "landing-web"');
    expect(m).toContain('"ghcr.io/apprafter/landing-web:latest"');
    expect(m).toContain('"apprafter.io/hostname":  "apprafter.dev"');
  });

  test('Application-preview.cue pins landing-web image + carries preview.apprafter.dev label', () => {
    const m = readFileSync(join(ROOT, 'apprafter/Application-preview.cue'), 'utf8');
    expect(m).toContain('name:      "landing-web-preview"');
    expect(m).toContain('"ghcr.io/apprafter/landing-web:latest"');
    expect(m).toContain('"apprafter.io/hostname": "preview.apprafter.dev"');
    // Same package so `cue vet ./apprafter/` covers both files in
    // one pass.
    expect(m).toContain('package apprafter');
  });
});

describe('container build surface (Phase J+)', () => {
  test('Dockerfile + Caddyfile present for web', () => {
    expect(existsSync(join(ROOT, 'Dockerfile'))).toBe(true);
    expect(existsSync(join(ROOT, 'Caddyfile'))).toBe(true);
  });

  test('web Dockerfile builds via fallback by default + accepts the override args', () => {
    const df = readFileSync(join(ROOT, 'Dockerfile'), 'utf8');
    // Default ARGs match the release path (release-landing.yml).
    expect(df).toMatch(/ARG LANDING_USE_FALLBACK=1/);
    expect(df).toMatch(/ARG LANDING_CMS_URL=https:\/\/cms\.apprafter\.dev/);
    expect(df).toMatch(/ARG PUBLIC_CMS_URL=https:\/\/cms\.apprafter\.dev/);
    // Rebuild path (rebuild-landing-web.yml) overrides the first
    // two — verify the build-arg surface exists.
    expect(df).toContain('ENV LANDING_USE_FALLBACK=${LANDING_USE_FALLBACK}');
    expect(df).toContain('ENV LANDING_CMS_URL=${LANDING_CMS_URL}');
    // Two-stage: bun builder + caddy runtime.
    expect(df).toMatch(/FROM oven\/bun.*AS builder/);
    expect(df).toMatch(/FROM caddy.*AS runtime/);
  });

  test('Caddyfile serves dist + 404 fallback + immutable cache for assets', () => {
    const cf = readFileSync(join(ROOT, 'Caddyfile'), 'utf8');
    expect(cf).toContain('/usr/share/caddy');
    expect(cf).toContain('try_files');
    expect(cf).toContain('/404.html');
    expect(cf).toContain('encode gzip zstd');
    expect(cf).toMatch(/\/fonts\/\*.*immutable/);
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

describe('honesty pass — tiers (2026-06-15)', () => {
  test('Tier 2 is no longer marked Available now; carries Phase 3', () => {
    const tl = readFileSync(join(ROOT, 'src/data/fallback/tierLadder.json'), 'utf8');
    const data = JSON.parse(tl);
    const t2 = data.cards.find((c: { num: string }) => c.num === 'Tier 2');
    expect(t2.status).toBe('roadmap');
    expect(t2.statusText).toContain('Phase 3');
    expect(t2.statusText).not.toContain('Available now');
  });

  test('TierLadder renders a waitlist button that dispatches the toggle with a preselect', () => {
    const c = readFileSync(join(ROOT, 'src/components/sections/TierLadder.astro'), 'utf8');
    expect(c).toContain('waitlist:toggle');
    expect(c).toContain('preselect');
    expect(c).toContain('data-preselect');
    // only T1 lights the stair now
    expect(c).toContain('n <= 1');
    expect(c).not.toContain('n <= 2');
  });

  test('advantages no longer claims Tier 2 / JetStream as available today', () => {
    const a = readFileSync(join(ROOT, 'src/data/fallback/advantages.json'), 'utf8');
    expect(a).not.toContain('Today: Tier 1 + Tier 2');
    expect(a).not.toContain('Works today on Tier 2 and above');
    expect(a).not.toContain('Today: Postgres · JetStream · Redis');
    expect(a).toContain('Phase 3');
  });

  test('scaling journey no longer says Tier 2 is ready at signup', () => {
    const s = readFileSync(join(ROOT, 'src/data/fallback/scalingJourney.json'), 'utf8');
    expect(s).not.toContain('Tier 1 and Tier 2 ready at signup');
    expect(s).toContain('ships in Phase 3');
  });
});
