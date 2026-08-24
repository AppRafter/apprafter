// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';
import registry from '../../../web/src/data/phases.json';

// A roadmap block's phase label must be one the public registry knows,
// so a CMS edit cannot introduce a phase label the landing chips /
// status page / subscribe form don't carry (the SYS-3 (b) source-time
// guard; layer (c)'s assertion 3 catches the same drift at runtime).
const REGISTRY_LABELS = new Set((registry.phases as Array<{ label: string }>).map((p) => p.label));

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
          validate: (value: string | null | undefined) => {
            if (typeof value !== 'string' || value.length === 0) {
              return 'A roadmap block must name its phase (e.g. "Phase 3").';
            }
            if (!REGISTRY_LABELS.has(value)) {
              return `"${value}" is not a known phase label. Use one of: ${[...REGISTRY_LABELS].join(', ')} (must match the phase registry).`;
            }
            return true;
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
