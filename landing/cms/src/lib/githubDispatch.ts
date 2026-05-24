// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { Payload } from 'payload';

/**
 * Fire a GitHub `repository_dispatch` event. Used by:
 *   - notifyRebuild      (eventType: 'landing-content-changed')
 *   - promoteToProd      (eventType: 'landing-promote-to-prod')
 *
 * Env requirements:
 *   GITHUB_DISPATCH_TOKEN  fine-grained PAT, Contents: write
 *   GITHUB_REPO            "owner/name", e.g. "AppRafter/apprafter"
 *
 * Without those vars the helper logs a warning and returns —
 * keeps dev iteration loops snappy without GitHub round-trips.
 */
export async function fireGithubDispatch(opts: {
  eventType: string;
  clientPayload?: Record<string, unknown>;
  payload: Payload;
}): Promise<{ ok: boolean; status: number | null }> {
  const token = process.env.GITHUB_DISPATCH_TOKEN;
  const repo = process.env.GITHUB_REPO;
  const { eventType, clientPayload, payload } = opts;

  if (!token || !repo) {
    payload.logger.warn(
      `[github-dispatch] GITHUB_DISPATCH_TOKEN / GITHUB_REPO not set — skipping ${eventType}`,
    );
    return { ok: false, status: null };
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
        event_type: eventType,
        client_payload: clientPayload ?? {},
      }),
    });
    if (!res.ok) {
      payload.logger.error(`[github-dispatch] ${eventType} returned ${res.status}`);
    } else {
      payload.logger.info(`[github-dispatch] ${eventType} fired (HTTP ${res.status})`);
    }
    return { ok: res.ok, status: res.status };
  } catch (err) {
    payload.logger.error({ err }, `[github-dispatch] ${eventType} failed`);
    return { ok: false, status: null };
  }
}
