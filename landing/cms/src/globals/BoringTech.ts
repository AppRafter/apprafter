// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const BoringTech: GlobalConfig = {
  slug: 'boring-tech',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'eyebrow', type: 'text', required: true, localized: true },
    { name: 'title', type: 'text', required: true, localized: true },
    { name: 'lede', type: 'textarea', required: true, localized: true },
    {
      name: 'underHoodLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Under the hood',
    },
    {
      name: 'ourCodeLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'What we wrote ourselves',
    },
    {
      name: 'underHood',
      type: 'array',
      labels: { singular: 'Under-the-hood item', plural: 'Under the hood' },
      fields: [
        { name: 'name', type: 'text', required: true },
        { name: 'desc', type: 'textarea', required: true, localized: true },
      ],
    },
    {
      name: 'ourCode',
      type: 'array',
      labels: { singular: 'Our-code item', plural: 'What we wrote ourselves' },
      fields: [
        { name: 'name', type: 'text', required: true },
        { name: 'desc', type: 'textarea', required: true, localized: true },
      ],
    },
    {
      name: 'closingHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: { description: 'HTML — emphasis via <strong>.' },
    },
  ],
};
