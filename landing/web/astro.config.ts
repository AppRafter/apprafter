// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import sitemap from '@astrojs/sitemap';
import svelte from '@astrojs/svelte';
import { defineConfig } from 'astro/config';

// Dev-time CMS upstream for the Vite proxy. Derived from the same
// LANDING_CMS_PORT env that drives `bun --filter @apprafter/landing-cms
// dev`, so changing the port in one place flows everywhere. Override
// LANDING_CMS_URL directly if Payload runs behind a reverse proxy
// during local dev.
const cmsPort = process.env.LANDING_CMS_PORT ?? '3000';
const cmsUrl = process.env.LANDING_CMS_URL ?? `http://localhost:${cmsPort}`;

export default defineConfig({
  site: 'https://apprafter.dev',
  integrations: [svelte(), sitemap()],
  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
    routing: { prefixDefaultLocale: false },
  },
  build: { inlineStylesheets: 'auto' },
  vite: {
    server: {
      proxy: {
        '/api': { target: cmsUrl, changeOrigin: true },
      },
    },
  },
});
