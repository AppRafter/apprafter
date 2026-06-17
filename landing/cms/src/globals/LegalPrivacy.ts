// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const LegalPrivacy: GlobalConfig = {
  slug: 'legal-privacy',
  admin: {
    description: 'Privacy page (apprafter.dev/privacy) — stub copy until the formal policy ships.',
  },
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'seoTitle', type: 'text', required: true, localized: true },
    { name: 'seoDescription', type: 'textarea', required: true, localized: true },
    { name: 'eyebrow', type: 'text', required: true, localized: true, defaultValue: '/ Legal' },
    { name: 'heading', type: 'text', required: true, localized: true, defaultValue: 'Privacy.' },
    {
      name: 'bodyHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: {
        description:
          'Raw HTML for the page body — lede paragraph, <h2>/<ul>/<p> sections, and the .closing note.',
      },
    },
  ],
};
