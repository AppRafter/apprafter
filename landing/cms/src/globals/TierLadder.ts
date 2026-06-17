// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const TierLadder: GlobalConfig = {
  slug: 'tier-ladder',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'eyebrow', type: 'text', required: true, localized: true },
    { name: 'title', type: 'text', required: true, localized: true },
    {
      name: 'waitlistButtonLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Join the waitlist',
    },
    {
      name: 'cards',
      type: 'array',
      minRows: 4,
      maxRows: 4,
      labels: { singular: 'Tier card', plural: 'Tier cards' },
      fields: [
        { name: 'num', type: 'text', required: true, localized: true },
        { name: 'title', type: 'text', required: true, localized: true },
        { name: 'price', type: 'text', required: true, localized: true },
        { name: 'desc', type: 'textarea', required: true, localized: true },
        {
          name: 'status',
          type: 'select',
          required: true,
          options: [
            { label: 'Live', value: 'live' },
            { label: 'Roadmap', value: 'roadmap' },
          ],
        },
        { name: 'statusText', type: 'text', required: true, localized: true },
      ],
    },
    {
      name: 'orthogonalNoteHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: { description: 'HTML — wrap "Confidential containers" in <strong>.' },
    },
  ],
};
