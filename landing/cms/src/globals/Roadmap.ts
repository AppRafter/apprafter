// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const Roadmap: GlobalConfig = {
  slug: 'roadmap',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'eyebrow', type: 'text', required: true, localized: true },
    { name: 'title', type: 'text', required: true, localized: true },
    { name: 'lede', type: 'textarea', required: true, localized: true },
    {
      name: 'phases',
      type: 'array',
      labels: { singular: 'Roadmap phase', plural: 'Roadmap phases' },
      fields: [
        {
          name: 'num',
          type: 'text',
          required: true,
          admin: {
            description:
              'e.g. "Phase 4", "Phase 8+" — the slug used for the section anchor id is derived from this string.',
          },
        },
        { name: 'title', type: 'text', required: true, localized: true },
        {
          name: 'items',
          type: 'array',
          fields: [{ name: 'text', type: 'text', required: true, localized: true }],
        },
      ],
    },
    { name: 'closing', type: 'textarea', required: true, localized: true },
  ],
};
