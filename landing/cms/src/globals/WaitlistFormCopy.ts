// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

export const WaitlistFormCopy: GlobalConfig = {
  slug: 'waitlist-form-copy',
  admin: {
    description:
      'Visible copy for the inline managed-launch waitlist form (Svelte island in Hero).',
  },
  access: {
    read: () => true,
    update: ({ req }) => Boolean(req.user),
  },
  fields: [
    { name: 'formIntro', type: 'textarea', required: true, localized: true },
    { name: 'emailLabel', type: 'text', required: true, localized: true, defaultValue: 'Email' },
    {
      name: 'useCaseLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: "What's your use case? (optional)",
    },
    {
      name: 'callLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: "I'd like a short call to discuss my use case.",
    },
    {
      name: 'submitLabel',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Notify me',
    },
    {
      name: 'successMessage',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: "→ We'll be in touch.",
    },
    {
      name: 'successWithCall',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: "→ We'll be in touch. You'll get a separate email with a calendar link.",
    },
    {
      name: 'storageNote',
      type: 'text',
      required: true,
      localized: true,
      defaultValue: 'Stored only for launch announcement.',
    },
  ],
};
