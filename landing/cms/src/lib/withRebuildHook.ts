// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalConfig } from 'payload';

import { notifyRebuild } from '../hooks/notifyRebuild';

/**
 * Append the notifyRebuild afterChange hook to a global config
 * without losing any hook the global already declares. Used in
 * payload.config.ts to wrap each content global so the static
 * web image gets re-rendered when copy changes.
 *
 * Booking is intentionally NOT wrapped — its content only flows
 * through the discovery-call email, never into the static page,
 * so rebuilding on Booking edits is pure waste.
 */
export function withRebuildHook<T extends GlobalConfig>(g: T): T {
  const existing = g.hooks?.afterChange ?? [];
  return {
    ...g,
    hooks: {
      ...g.hooks,
      afterChange: [...existing, notifyRebuild],
    },
  };
}
