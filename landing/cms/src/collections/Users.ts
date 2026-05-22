// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { CollectionConfig } from 'payload';

/**
 * Admin user collection. Default Payload auth — email + password.
 * Only admins exist; no public registration is exposed by the site.
 */
export const Users: CollectionConfig = {
  slug: 'users',
  auth: true,
  admin: {
    useAsTitle: 'email',
    defaultColumns: ['email', 'name'],
  },
  fields: [
    {
      name: 'name',
      type: 'text',
    },
  ],
};
