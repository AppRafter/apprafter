// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const LandingTransparency: GlobalConfig = {
  slug: 'landing-transparency',
  admin: {
    description:
      'Three cards below the Comparison table: 4.5.1 Pricing / 4.5.2 Anti-lock / 4.5.3 Alignment.',
  },
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      name: 'blocks',
      type: 'array',
      minRows: 3,
      maxRows: 3,
      fields: [
        { name: 'kicker', type: 'text', required: true, localized: true },
        { name: 'title', type: 'text', required: true, localized: true },
        {
          name: 'bodyHtml',
          type: 'textarea',
          required: true,
          localized: true,
          admin: {
            description:
              'HTML — wrap paragraphs in <p>…</p>, use <strong> for emphasis, <code> for inline tokens.',
          },
        },
      ],
    },
  ],
};
