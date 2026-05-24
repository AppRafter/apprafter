// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

import { promoteToProd } from '../hooks/promoteToProd';

/**
 * Preview ↔ prod promotion control + version-diff tracker.
 *
 * Edit flow (already wired by notifyRebuild on every content global):
 *   admin saves any content global  → stamps lastEditAt +
 *                                      lastEditedGlobal here +
 *                                      fires :preview rebuild
 *
 * Promote flow:
 *   admin opens Publishing → ticks promoteToProd → Save
 *     → beforeChange (promoteToProd hook) fires the
 *       landing-promote-to-prod dispatch
 *     → stamps lastPromotedAt with now()
 *     → resets promoteToProd back to false
 *
 * "Is preview ahead of prod?"
 *   The admin reads it as: lastEditAt > lastPromotedAt.
 *
 * Access is admin-only — these timestamps reveal edit cadence,
 * which is internal. The static page never references this
 * global, so it is NOT wrapped with the notifyRebuild hook in
 * payload.config.ts.
 */
export const Publishing: GlobalConfig = {
  slug: 'publishing',
  admin: {
    description:
      'Preview ↔ prod promotion control. Tick `promoteToProd` and save to retag :preview → :prod. The checkbox auto-resets after each save.',
  },
  access: {
    read: ({ req }) => Boolean(req.user),
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      type: 'row',
      fields: [
        {
          name: 'lastEditAt',
          type: 'date',
          admin: {
            readOnly: true,
            description: 'Latest content-global save (stamped by notifyRebuild).',
            width: '50%',
          },
        },
        {
          name: 'lastEditedGlobal',
          type: 'text',
          admin: {
            readOnly: true,
            description: 'Slug of the global that was edited last.',
            width: '50%',
          },
        },
      ],
    },
    {
      name: 'lastPromotedAt',
      type: 'date',
      admin: {
        readOnly: true,
        description:
          'Most recent successful Promote-to-prod. Preview is ahead of prod when lastEditAt is later than this.',
      },
    },
    {
      name: 'promoteToProd',
      type: 'checkbox',
      defaultValue: false,
      admin: {
        description:
          'Tick + Save to retag ghcr.io/<owner>/landing-web:preview → :prod (+ :latest). Auto-resets to false after save.',
      },
    },
    {
      name: 'editLog',
      type: 'array',
      // Capped — last 20 entries since the most recent promote.
      // Each Promote-to-prod clears the array (see promoteToProd
      // hook), so the visible list answers: "what's been edited
      // since prod was last refreshed?"
      maxRows: 20,
      labels: {
        singular: 'Edit',
        plural: 'Edits since last promote',
      },
      admin: {
        readOnly: true,
        description:
          'Last 20 content edits since the most recent promote. Cleared on Promote-to-prod.',
        initCollapsed: true,
      },
      fields: [
        {
          type: 'row',
          fields: [
            {
              name: 'at',
              type: 'date',
              required: true,
              admin: { readOnly: true, width: '40%' },
            },
            {
              name: 'global',
              type: 'text',
              required: true,
              admin: { readOnly: true, width: '40%' },
            },
            {
              name: 'editor',
              type: 'text',
              admin: { readOnly: true, width: '20%' },
            },
          ],
        },
      ],
    },
  ],
  hooks: {
    beforeChange: [promoteToProd],
  },
};
