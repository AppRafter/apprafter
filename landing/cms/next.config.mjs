// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import { withPayload } from '@payloadcms/next/withPayload';

/** @type {import('next').NextConfig} */
const nextConfig = {
  // Sharp must run in the Node runtime, not Edge.
  serverExternalPackages: ['sharp'],

  // No public-facing frontend on the CMS — admin only.
  // Visiting / redirects to /admin so the bare host still does something useful.
  async redirects() {
    return [{ source: '/', destination: '/admin', permanent: false }];
  },
};

export default withPayload(nextConfig);
