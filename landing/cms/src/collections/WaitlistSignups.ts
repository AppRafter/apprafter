// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { CollectionConfig } from 'payload';

import { sendDiscoveryEmail } from '../hooks/sendDiscoveryEmail';

/**
 * Pre-launch waitlist storage. Submitted unauthenticated from
 * landing/web's WaitlistForm island; each signup records which
 * upcoming releases it wants to hear about (`interests`). The
 * afterChange hook sends a Calendly link when `wantsCall` is true
 * and SMTP is configured; without SMTP it logs the intent and
 * moves on so dev signups don't fail.
 */
export const WaitlistSignups: CollectionConfig = {
  slug: 'waitlist-signups',
  admin: {
    useAsTitle: 'email',
    defaultColumns: ['email', 'wantsCall', 'interests', 'callEmailSentAt', 'createdAt'],
    description:
      'Launch waitlist. Email is the unique key; interests record which upcoming releases the signup wants; wantsCall toggles a follow-up Calendly invitation.',
  },
  access: {
    // Anyone can submit (no auth on the public site); only admins
    // can read, update, delete.
    create: () => true,
    read: ({ req }) => Boolean(req.user),
    update: ({ req }) => Boolean(req.user),
    delete: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      name: 'email',
      type: 'email',
      required: true,
      unique: true,
      index: true,
    },
    {
      name: 'useCase',
      type: 'text',
      admin: { description: 'Optional one-liner — Mary, 3 services, on Render' },
    },
    {
      name: 'interests',
      type: 'select',
      hasMany: true,
      options: [
        { label: 'Tier 2 — production multi-node (Phase 3)', value: 'tier2' },
        { label: 'Observability (Phase 3)', value: 'observability' },
        { label: 'Managed offering (Phase 4)', value: 'managed' },
        { label: 'Tier 3 — bare metal (Phase 5+)', value: 'tier3' },
        { label: 'Tier 4 — confidential (Phase 6+)', value: 'tier4' },
      ],
      admin: { description: 'Which upcoming releases this signup wants to hear about.' },
    },
    {
      name: 'wantsCall',
      type: 'checkbox',
      defaultValue: false,
      admin: { description: 'Triggers a Calendly link email via afterChange hook.' },
    },
    {
      name: 'source',
      type: 'text',
      admin: {
        readOnly: true,
        description: 'document.referrer at submit time — empty means direct visit.',
      },
    },
    {
      name: 'callEmailSentAt',
      type: 'date',
      admin: {
        readOnly: true,
        description: 'Stamped by the hook after the Calendly email is sent.',
      },
    },
  ],
  hooks: {
    afterChange: [sendDiscoveryEmail],
  },
  timestamps: true,
};
