// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const LegalTerms: GlobalConfig = {
  slug: 'legal-terms',
  admin: {
    description: 'Terms page (apprafter.dev/terms) — stub copy until the formal terms ship.',
  },
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'seoTitle', type: 'text', required: true, localized: true },
    { name: 'seoDescription', type: 'textarea', required: true, localized: true },
    { name: 'eyebrow', type: 'text', required: true, localized: true, defaultValue: '/ Legal' },
    { name: 'heading', type: 'text', required: true, localized: true, defaultValue: 'Terms.' },
    {
      name: 'bodyHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: {
        description:
          'Raw HTML for the page body — lede paragraph, <h2>/<p> sections, and the .closing note.',
      },
    },
  ],
};
