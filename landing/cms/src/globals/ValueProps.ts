// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const ValueProps: GlobalConfig = {
  slug: 'value-props',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      name: 'eyebrow',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Why AppRafter',
    },
    {
      name: 'title',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: "Three things you don't have to fight anymore.",
    },
    {
      name: 'blocks',
      type: 'array',
      minRows: 3,
      maxRows: 3,
      labels: { singular: 'Value block', plural: 'Value blocks' },
      fields: [
        { name: 'title', type: 'text', required: true, localized: true },
        {
          name: 'bodyHtml',
          type: 'textarea',
          required: true,
          localized: true,
          admin: {
            description:
              'Raw HTML — inline <code class="mono"> spans for code identifiers are supported.',
          },
        },
        {
          name: 'iconName',
          type: 'select',
          required: true,
          options: [
            { label: 'Grid (single manifest)', value: 'grid' },
            { label: 'Bars (scaling tiers)', value: 'bars' },
            { label: 'Lock (open source)', value: 'lock' },
          ],
        },
      ],
    },
  ],
};
