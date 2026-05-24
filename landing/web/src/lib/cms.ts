// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type * as T from './types';

/**
 * Resolve the upstream Payload URL.
 *
 *   PUBLIC_CMS_URL      browser-exposed prod origin (e.g.
 *                       https://cms.apprafter.dev). Set in CI/prod.
 *   LANDING_CMS_URL     dev override for the Vite proxy target.
 *   LANDING_CMS_PORT    dev port (default 3000).
 *
 * In dev, leave PUBLIC_CMS_URL empty so fetches go to relative
 * /api/* and the Astro Vite proxy forwards them. In prod, set
 * PUBLIC_CMS_URL to the absolute CMS origin.
 */
const RAW_PUBLIC = import.meta.env.PUBLIC_CMS_URL;
const PUBLIC_CMS_URL = typeof RAW_PUBLIC === 'string' ? RAW_PUBLIC : '';

const cmsPort = process.env.LANDING_CMS_PORT ?? '3000';
const SERVER_CMS_URL = process.env.LANDING_CMS_URL ?? `http://localhost:${cmsPort}`;

/** Pre-bundle every JSON in src/data/fallback so Vite can serve them
 *  without dynamic-import resolution at runtime. */
const fallbackModules = import.meta.glob<{ default: unknown }>('../data/fallback/*.json', {
  eager: true,
});

function loadFallback<U>(name: string): U {
  const key = `../data/fallback/${name}.json`;
  const mod = fallbackModules[key];
  if (!mod) {
    throw new Error(`[cms] missing fallback JSON for "${name}"`);
  }
  return mod.default as U;
}

/**
 * Build-time opt-in to use the fallback JSONs even in prod. Set
 * `LANDING_USE_FALLBACK=1` during `astro build` to skip the CMS
 * fetch entirely — pages render from web/src/data/fallback/*.json.
 *
 * This is the path the Docker build takes: the image is reproducible
 * without a running Payload reachable from the CI runner, and the
 * resulting static output uses the JSONs that ship in the repo.
 *
 * Operators who want CMS-driven static builds (rebuild after edits
 * in admin) leave LANDING_USE_FALLBACK unset and run the build
 * inside the cluster where the CMS is reachable.
 */
const USE_FALLBACK = (process.env.LANDING_USE_FALLBACK ?? '') === '1';

async function fetchGlobal<U>(slug: string, fallbackName: string): Promise<U> {
  if (USE_FALLBACK) {
    return loadFallback<U>(fallbackName);
  }

  // Astro SSR runs on the server — relative URLs won't work there.
  // Use SERVER_CMS_URL (resolved via env at build/SSR time) for the
  // base; the browser-side islands set PUBLIC_CMS_URL to make
  // direct requests in production, and rely on the Vite proxy in dev.
  const base = import.meta.env.SSR ? SERVER_CMS_URL : PUBLIC_CMS_URL;
  const url = `${base}/api/globals/${slug}?depth=2&locale=en`;

  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`CMS /globals/${slug} → ${res.status}`);
    return (await res.json()) as U;
  } catch (err) {
    if (import.meta.env.PROD) {
      // Prod must not silently fall back — build should fail loudly
      // so we don't ship a stale or partial page (unless the
      // operator opted in via LANDING_USE_FALLBACK=1, handled above).
      throw err;
    }
    if (import.meta.env.DEV) {
      console.warn(`[cms] dev fallback for /globals/${slug}:`, err);
    }
    return loadFallback<U>(fallbackName);
  }
}

export const getSiteSettings = () => fetchGlobal<T.SiteSetting>('site-settings', 'siteSettings');

export const getLandingHero = () => fetchGlobal<T.LandingHero>('landing-hero', 'landingHero');

export const getValueProps = () => fetchGlobal<T.ValueProp>('value-props', 'valueProps');

export const getScalingJourney = () =>
  fetchGlobal<T.ScalingJourney>('scaling-journey', 'scalingJourney');

export const getTierLadder = () => fetchGlobal<T.TierLadder>('tier-ladder', 'tierLadder');

export const getComparison = () => fetchGlobal<T.Comparison>('comparison', 'comparison');

export const getTransparency = () =>
  fetchGlobal<T.LandingTransparency>('landing-transparency', 'transparency');

export const getBoringTech = () => fetchGlobal<T.BoringTech>('boring-tech', 'boringTech');

export const getAdvantages = () => fetchGlobal<T.Advantage>('advantages', 'advantages');

export const getRoadmap = () => fetchGlobal<T.Roadmap>('roadmap', 'roadmap');

export const getBootstrapStrip = () =>
  fetchGlobal<T.BootstrapStrip>('bootstrap-strip', 'bootstrapStrip');

export const getFooterContent = () => fetchGlobal<T.FooterContent>('footer-content', 'footer');

export const getWaitlistCopy = () =>
  fetchGlobal<T.WaitlistFormCopy>('waitlist-form-copy', 'waitlistCopy');

/** Substitute {{year}} in the footer copyright string. */
export function renderCopyright(template: string, year = new Date().getFullYear()): string {
  return template.replace(/\{\{year\}\}/g, String(year));
}
