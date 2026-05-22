// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const ScalingJourney: GlobalConfig = {
  slug: 'scaling-journey',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'eyebrow', type: 'text', required: true, localized: true },
    { name: 'title', type: 'text', required: true, localized: true },
    {
      name: 'ledeHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: { description: 'HTML — <code class="mono"> supported.' },
    },
    {
      name: 'leftStage',
      type: 'group',
      fields: [
        { name: 'eyebrow', type: 'text', required: true, localized: true },
        { name: 'fileName', type: 'text', required: true, defaultValue: 'Application.cue' },
        { name: 'fileMeta', type: 'text', required: true, localized: true },
        { name: 'caption', type: 'textarea', required: true, localized: true },
      ],
    },
    {
      name: 'rightStage',
      type: 'group',
      fields: [
        { name: 'eyebrow', type: 'text', required: true, localized: true },
        { name: 'fileName', type: 'text', required: true, defaultValue: 'Application.cue' },
        { name: 'fileMeta', type: 'text', required: true, localized: true },
        { name: 'caption', type: 'textarea', required: true, localized: true },
      ],
    },
    {
      name: 'arrowLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Tier upgrade',
    },
    {
      name: 'footerKickers',
      type: 'array',
      minRows: 1,
      maxRows: 6,
      fields: [{ name: 'text', type: 'text', required: true, localized: true }],
    },
    {
      name: 'caveatHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: { description: 'HTML — wrap the leading "Today:" in <strong>.' },
    },
  ],
};
