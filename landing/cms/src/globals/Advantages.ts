// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const Advantages: GlobalConfig = {
  slug: 'advantages',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'eyebrow', type: 'text', required: true, localized: true },
    { name: 'title', type: 'text', required: true, localized: true },
    { name: 'lede', type: 'textarea', required: true, localized: true },
    {
      name: 'blocks',
      type: 'array',
      labels: { singular: 'KFM block', plural: 'KFM blocks' },
      fields: [
        { name: 'title', type: 'text', required: true, localized: true },
        {
          name: 'leadHtml',
          type: 'textarea',
          required: true,
          localized: true,
          admin: {
            description:
              'HTML — open with a <strong> claim, may contain inline <code class="mono">.',
          },
        },
        { name: 'detail', type: 'textarea', required: true, localized: true },
        { name: 'phaseTag', type: 'text', required: true, localized: true },
        {
          name: 'featured',
          type: 'checkbox',
          defaultValue: false,
          admin: {
            description: 'Spans the full row as a closer card. Use sparingly — one per section.',
          },
        },
      ],
    },
  ],
};
