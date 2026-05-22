// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

/**
 * Discovery-call booking config — read by sendDiscoveryEmail when
 * a WaitlistSignups doc has wantsCall=true. Per Q3 (2026-05-22):
 * we use Calendly as the default scheduling service; the URL is
 * editable here so swapping providers (Cal.com, SavvyCal, …) is
 * a one-field change instead of a code release.
 */
export const Booking: GlobalConfig = {
  slug: 'booking',
  admin: {
    description:
      'Discovery-call booking URL + email template for waitlist signups that ticked "I’d like a short call".',
  },
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      name: 'discoveryCallUrl',
      type: 'text',
      required: true,
      defaultValue: 'https://calendly.com/apprafter/discovery',
      admin: {
        description: 'Calendly (or compatible) link sent to wantsCall=true signups.',
      },
    },
    {
      name: 'discoveryCallEmailSubject',
      type: 'text',
      required: true,
      defaultValue: 'AppRafter — pick a slot for a discovery call',
      localized: true,
    },
    {
      name: 'discoveryCallEmailBody',
      type: 'textarea',
      required: true,
      defaultValue:
        'Thanks for signing up. Pick a 30-minute slot here:\n\n{{url}}\n\nWe’ll dig into your use case — no pitch deck, no slides, just a conversation.\n\n— AppRafter',
      localized: true,
      admin: {
        description:
          'Plaintext email body. {{url}} is replaced with discoveryCallUrl at send time.',
      },
    },
  ],
};
