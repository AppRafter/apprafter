// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalAfterChangeHook } from 'payload';

/**
 * Fires a GitHub `repository_dispatch` event when any content
 * global changes, so .github/workflows/rebuild-landing-web.yml
 * picks the edit up and rebuilds the static web image with
 * LANDING_USE_FALLBACK=0 + LANDING_CMS_URL=<live>. The release
 * workflow on `landing-v*` tags is unaffected — that path stays
 * deterministic via the JSON fallbacks.
 *
 * Env requirements (set in cms/.env on the deploy host):
 *
 *   GITHUB_DISPATCH_TOKEN   fine-grained PAT with
 *                           "Repository permissions > Contents:
 *                           write" on the apprafter repo. Or a
 *                           classic PAT with `repo` scope.
 *   GITHUB_REPO             "AppRafter/apprafter" (owner/name).
 *
 * Without those vars the hook logs a warning and returns — admin
 * edits still persist, the rebuild just doesn't fire. Useful in
 * dev where you're hammering globals while iterating on copy.
 *
 * Debouncing: GitHub Actions handles coalescing via the workflow's
 * `concurrency: { group: ..., cancel-in-progress: true }` setting,
 * so we don't bother throttling here. Multiple saves in quick
 * succession → multiple dispatches → only the latest build
 * completes; the rest are aborted by Actions.
 */
export const notifyRebuild: GlobalAfterChangeHook = async ({ doc, global, req }) => {
  const token = process.env.GITHUB_DISPATCH_TOKEN;
  const repo = process.env.GITHUB_REPO;
  if (!token || !repo) {
    req.payload.logger.warn(
      `[notifyRebuild] GITHUB_DISPATCH_TOKEN / GITHUB_REPO not set — skipping rebuild dispatch for ${global.slug}`,
    );
    return doc;
  }

  try {
    const res = await fetch(`https://api.github.com/repos/${repo}/dispatches`, {
      method: 'POST',
      headers: {
        Accept: 'application/vnd.github+json',
        Authorization: `Bearer ${token}`,
        'X-GitHub-Api-Version': '2022-11-28',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        event_type: 'landing-content-changed',
        client_payload: {
          source: 'payload-cms',
          global: global.slug,
          at: new Date().toISOString(),
        },
      }),
    });
    if (!res.ok) {
      req.payload.logger.error(
        `[notifyRebuild] dispatch returned ${res.status} for ${global.slug}`,
      );
    } else {
      req.payload.logger.info(
        `[notifyRebuild] dispatch fired for ${global.slug} (HTTP ${res.status})`,
      );
    }
  } catch (err) {
    req.payload.logger.error({ err }, '[notifyRebuild] dispatch failed');
  }

  return doc;
};
