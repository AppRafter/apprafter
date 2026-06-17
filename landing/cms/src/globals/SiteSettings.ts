// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const SiteSettings: GlobalConfig = {
  slug: 'site-settings',
  admin: {
    description:
      'Cross-page site config: external links exposed in nav/footer + (later) analytics domain.',
  },
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      name: 'githubUrl',
      type: 'text',
      required: true,
      defaultValue: 'https://github.com/AppRafter/apprafter',
    },
    {
      name: 'specUrl',
      type: 'text',
      required: true,
      defaultValue: 'https://github.com/AppRafter/apprafter/blob/master/spec.md',
    },
    {
      name: 'docsUrl',
      type: 'text',
      admin: {
        description:
          'Empty renders the nav Docs link with a "Soon" badge pointing to GitHub README.',
      },
    },
    {
      name: 'plausibleDomain',
      type: 'text',
      admin: {
        description: 'Reserved for Phase I+ analytics. Leave empty for v1.',
      },
    },
    {
      name: 'brandName',
      type: 'text',
      required: true,
      defaultValue: 'AppRafter',
      admin: { description: 'Full brand name — og:site_name + the mono wordmark.' },
    },
    {
      name: 'brandNameAccent',
      type: 'text',
      required: true,
      defaultValue: 'Rafter',
      admin: {
        description:
          'Trailing slice of brandName rendered in the accent colour in the two-tone wordmark.',
      },
    },
    { name: 'navDocsLabel', type: 'text', required: true, localized: true, defaultValue: 'Docs' },
    { name: 'navSpecLabel', type: 'text', required: true, localized: true, defaultValue: 'Spec' },
    { name: 'soonBadgeLabel', type: 'text', required: true, localized: true, defaultValue: 'Soon' },
    {
      name: 'homeSeoTitle',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'AppRafter — One manifest. From a €5 VPS to production. Open source.',
      admin: { description: 'The <title> for the home page.' },
    },
    {
      name: 'homeSeoDescription',
      type: 'textarea',
      required: true,
      localized: true,
      defaultValue:
        'AppRafter is an opinionated PaaS on Kubernetes. Describe your applications in a single CUE manifest — the same one runs from a single VDS to a multi-node production cluster. Open source (FSL-1.1-Apache-2.0).',
      admin: { description: 'The meta description for the home page.' },
    },
  ],
};
