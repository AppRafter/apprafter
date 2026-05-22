// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const LandingHero: GlobalConfig = {
  slug: 'landing-hero',
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      name: 'headlineHtml',
      type: 'textarea',
      required: true,
      localized: true,
      admin: {
        description: 'Raw HTML — wrap accented words in <span class="accented">…</span>.',
      },
    },
    { name: 'subhead', type: 'textarea', required: true, localized: true },
    { name: 'statusBadge', type: 'text', required: true, localized: true },
    { name: 'cueFilename', type: 'text', required: true, defaultValue: 'billing-api.cue' },
    {
      name: 'cueSnippet',
      type: 'code',
      required: true,
      admin: { language: 'yaml' },
    },
    {
      name: 'primaryCTA',
      type: 'group',
      fields: [
        { name: 'label', type: 'text', required: true, localized: true },
        { name: 'href', type: 'text', required: true },
      ],
    },
    {
      name: 'secondaryCTA',
      type: 'group',
      admin: {
        description: 'Opens the inline waitlist form — no href needed, just a label.',
      },
      fields: [{ name: 'label', type: 'text', required: true, localized: true }],
    },
    {
      name: 'tertiaryCTA',
      type: 'group',
      fields: [
        { name: 'label', type: 'text', required: true, localized: true },
        { name: 'href', type: 'text', required: true },
      ],
    },
  ],
};
