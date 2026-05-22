// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

/// <reference path="../.astro/types.d.ts" />
/// <reference types="astro/client" />

// Browser-side env (Astro exposes PUBLIC_* and Vite-defined keys
// via import.meta.env).
interface ImportMetaEnv {
  readonly PUBLIC_CMS_URL: string;
  readonly CMS_API_KEY: string | undefined;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

// Build- and dev-time env (read via process.env from astro.config.ts
// and package.json scripts). All optional — defaults live in
// astro.config.ts and the dev/preview scripts.
declare namespace NodeJS {
  interface ProcessEnv {
    /** Port for `bun run dev` (Astro dev server). Default: 4321. */
    readonly LANDING_WEB_PORT?: string;
    /** Port for `bun run preview` (Astro preview server). Default: 4322. */
    readonly LANDING_WEB_PREVIEW_PORT?: string;
    /** Port where Payload (Next.js) is listening — drives the Vite
     *  /api proxy target. Default: 3000. */
    readonly LANDING_CMS_PORT?: string;
    /** Full URL of the Payload upstream. Default derives from
     *  LANDING_CMS_PORT (`http://localhost:${LANDING_CMS_PORT}`).
     *  Override when Payload sits behind a reverse proxy in dev. */
    readonly LANDING_CMS_URL?: string;
  }
}
