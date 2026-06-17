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
  ],
};
