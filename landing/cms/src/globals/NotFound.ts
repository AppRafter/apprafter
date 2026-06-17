// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const NotFound: GlobalConfig = {
  slug: 'not-found',
  admin: {
    description: 'Copy for the 404 page (apprafter.dev/404).',
  },
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'seoTitle', type: 'text', required: true, localized: true },
    { name: 'seoDescription', type: 'textarea', required: true, localized: true },
    { name: 'eyebrow', type: 'text', required: true, localized: true, defaultValue: '/ 404' },
    {
      name: 'heading',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Page not found.',
    },
    {
      name: 'bodyHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: { description: 'Raw HTML for the lede paragraph under the 404 heading.' },
    },
  ],
};
