// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const Comparison: GlobalConfig = {
  slug: 'comparison',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'eyebrow', type: 'text', required: true, localized: true },
    { name: 'title', type: 'text', required: true, localized: true },
    {
      name: 'columns',
      type: 'group',
      admin: { description: 'Three-column header labels + status pills.' },
      fields: [
        {
          name: 'self',
          type: 'group',
          fields: [
            {
              name: 'label',
              type: 'text',
              required: true,
              localized: true,
              defaultValue: 'Self-host',
            },
            {
              name: 'statusKind',
              type: 'select',
              required: true,
              defaultValue: 'live',
              options: ['live', 'waitlist', 'roadmap'],
            },
            {
              name: 'statusLabel',
              type: 'text',
              required: true,
              localized: true,
              defaultValue: 'Available now',
            },
          ],
        },
        {
          name: 'managed',
          type: 'group',
          fields: [
            {
              name: 'label',
              type: 'text',
              required: true,
              localized: true,
              defaultValue: 'Managed',
            },
            { name: 'badgeSuffix', type: 'text', localized: true, defaultValue: '(waitlist)' },
            {
              name: 'statusKind',
              type: 'select',
              required: true,
              defaultValue: 'waitlist',
              options: ['live', 'waitlist', 'roadmap'],
            },
            {
              name: 'statusLabel',
              type: 'text',
              required: true,
              localized: true,
              defaultValue: 'Waitlist',
            },
          ],
        },
        {
          name: 'turnkey',
          type: 'group',
          fields: [
            {
              name: 'label',
              type: 'text',
              required: true,
              localized: true,
              defaultValue: 'Turnkey',
            },
            { name: 'badgeSuffix', type: 'text', localized: true, defaultValue: '(roadmap)' },
            {
              name: 'statusKind',
              type: 'select',
              required: true,
              defaultValue: 'roadmap',
              options: ['live', 'waitlist', 'roadmap'],
            },
            {
              name: 'statusLabel',
              type: 'text',
              required: true,
              localized: true,
              defaultValue: 'Roadmap',
            },
          ],
        },
      ],
    },
    {
      name: 'statusRowLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Status',
    },
    {
      name: 'managedWaitlistButtonLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Join waitlist',
    },
    {
      name: 'rows',
      type: 'array',
      minRows: 1,
      labels: { singular: 'Row', plural: 'Rows' },
      fields: [
        { name: 'label', type: 'text', required: true, localized: true },
        { name: 'selfHtml', type: 'textarea', required: true, localized: true },
        { name: 'managedHtml', type: 'textarea', required: true, localized: true },
        { name: 'turnkeyHtml', type: 'textarea', required: true, localized: true },
      ],
    },
    {
      name: 'footnoteHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: { description: 'HTML — uses <sup>†</sup> for the dagger marker.' },
    },
  ],
};
