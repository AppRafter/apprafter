// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const BootstrapStrip: GlobalConfig = {
  slug: 'bootstrap-strip',
  admin: {
    description: 'One italic line above the footer — bootstrap-funded / no-VC angle.',
  },
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    {
      name: 'body',
      type: 'textarea',
      required: true,
      localized: true,
    },
  ],
};
