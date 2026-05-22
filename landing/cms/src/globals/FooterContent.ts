// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const FooterContent: GlobalConfig = {
  slug: 'footer-content',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      name: 'brandDesc',
      type: 'textarea',
      required: true,
      localized: true,
    },
    {
      name: 'columns',
      type: 'array',
      minRows: 1,
      labels: { singular: 'Footer column', plural: 'Footer columns' },
      fields: [
        { name: 'heading', type: 'text', required: true, localized: true },
        {
          name: 'links',
          type: 'array',
          fields: [
            { name: 'label', type: 'text', required: true, localized: true },
            { name: 'href', type: 'text', required: true },
            {
              name: 'external',
              type: 'checkbox',
              defaultValue: false,
              admin: { description: 'Renders with target="_blank" rel="noreferrer".' },
            },
            {
              name: 'soon',
              type: 'checkbox',
              defaultValue: false,
              admin: { description: 'Appends a small "SOON" badge.' },
            },
          ],
        },
      ],
    },
    {
      name: 'copyright',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: '© {{year}} AppRafter · apprafter.dev',
      admin: { description: '{{year}} is replaced with the current year at render time.' },
    },
    {
      name: 'licenseNote',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'FSL-1.1-Apache-2.0 · auto-converts to Apache 2.0 after 2 years',
    },
    {
      name: 'founderNote',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Bootstrap-funded. Built solo.',
    },
  ],
};
