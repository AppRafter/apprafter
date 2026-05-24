// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { withPayload } from '@payloadcms/next/withPayload';

const dirname = path.dirname(fileURLToPath(import.meta.url));

/** @type {import('next').NextConfig} */
const nextConfig = {
  // Sharp must run in the Node runtime, not Edge.
  serverExternalPackages: ['sharp'],

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
