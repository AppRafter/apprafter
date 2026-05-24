// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalAfterChangeHook } from 'payload';

import { fireGithubDispatch } from '../lib/githubDispatch';

/**
 * Two-step fan-out on every content-global save:
 *
 *   1. Stamp Publishing.lastEditAt + lastEditedGlobal so the admin
 *      UI can show "preview is N minutes ahead of prod" without
 *      reaching out to GHCR.
 *   2. Fire a GitHub `repository_dispatch` of type
 *      `landing-content-changed`, which
 *      .github/workflows/landing-preview-build.yml picks up. That
 *      workflow rebuilds the web image and pushes it as :preview
 *      (+ :preview-<sha>).
 *
 * The promote-to-prod flow (Publishing.promoteToProd checkbox →
 * landing-promote-to-prod.yml retags :preview → :prod) is
 * intentionally a separate path so saves never accidentally
 * deploy to prod.
 *
 * No infinite loop: Publishing is NOT wrapped with this hook
 * (see payload.config.ts) — its own beforeChange handles promote
 * logic.
 */
export const notifyRebuild: GlobalAfterChangeHook = async ({ doc, global, req }) => {
  // Step 1 — Publishing tracker (best-effort, doesn't block dispatch).
  try {
    await req.payload.updateGlobal({
      slug: 'publishing',
      data: {
        lastEditAt: new Date().toISOString(),
        lastEditedGlobal: global.slug,
      },
      // Tag so Publishing's own hooks can ignore this internal update
      // if/when we add them.
      context: { fromNotifyRebuild: true },
      depth: 0,
      overrideAccess: true,
    });
  } catch (err) {
    req.payload.logger.warn(
      { err },
      `[notifyRebuild] Publishing.lastEditAt update failed for ${global.slug} (preview will still fire)`,
    );
  }

  // Step 2 — fire the preview-build dispatch.
  await fireGithubDispatch({
    eventType: 'landing-content-changed',
    clientPayload: {
      source: 'payload-cms',
      global: global.slug,
      at: new Date().toISOString(),
    },
    payload: req.payload,
  });

  return doc;
};
