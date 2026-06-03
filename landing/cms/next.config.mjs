// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { withPayload } from '@payloadcms/next/withPayload';

const dirname = path.dirname(fileURLToPath(import.meta.url));

/** @type {import('next').NextConfig} */
const nextConfig = {
  // Sharp must run in the Node runtime, not Edge. `undici` is kept
  // external too: Payload's upload SSRF guard (payload/dist/uploads/
  // safeFetch) imports it, and undici does not webpack-bundle
  // cleanly (a lazy `require('undici')` survives bundling and then
  // 500s at runtime with MODULE_NOT_FOUND). Externalizing keeps a
  // single undici instance resolved from node_modules — which is why
  // it is also a direct dependency in package.json (mirrors how
  // withPayload externalizes graphql, itself a direct dep).
  serverExternalPackages: ['sharp', 'undici'],

  // No public-facing frontend on the CMS — admin only.
  // Visiting / redirects to /admin so the bare host still does
  // something useful.
  async redirects() {
    return [{ source: '/', destination: '/admin', permanent: false }];
  },

  // Build a minimal runtime tree at `.next/standalone/` so the
  // Docker image doesn't have to copy the entire node_modules.
  // `outputFileTracingRoot` points at the Bun workspace root so
  // Next picks up hoisted dependencies from `landing/node_modules`.
  output: 'standalone',
  outputFileTracingRoot: path.join(dirname, '../'),
};

export default withPayload(nextConfig);
