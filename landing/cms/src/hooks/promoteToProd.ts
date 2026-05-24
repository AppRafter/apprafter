// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { GlobalBeforeChangeHook } from 'payload';

import { fireGithubDispatch } from '../lib/githubDispatch';

/**
 * beforeChange on Publishing. When the admin saves with
 * `promoteToProd === true`:
 *
 *   1. Fire a `landing-promote-to-prod` repository_dispatch.
 *      The workflow retags ghcr.io/.../landing-web:preview →
 *      :prod + :latest via `docker buildx imagetools create`
 *      (no rebuild — byte-identical image, single registry blob).
 *   2. Stamp lastPromotedAt with the current time.
 *   3. Reset promoteToProd to false so the checkbox is a
 *      one-shot — admin has to re-tick it to fire again.
 *
 * Dispatch is best-effort: a GitHub outage / missing token
 * shouldn't block the admin save. The error is logged, the
 * timestamp is still stamped (which is honest: the admin asked
 * to promote; whether the workflow ran is on the operator to
 * verify). If you prefer fail-loud, swap to `throw` below and
 * remove the surrounding try.
 */
export const promoteToProd: GlobalBeforeChangeHook = async ({ data, req }) => {
  if (!data || data.promoteToProd !== true) return data;

  try {
    await fireGithubDispatch({
      eventType: 'landing-promote-to-prod',
      clientPayload: {
        source: 'payload-cms',
        triggeredBy: req.user?.email ?? 'unknown',
        at: new Date().toISOString(),
      },
      payload: req.payload,
    });
  } catch (err) {
    req.payload.logger.error({ err }, '[promoteToProd] dispatch failed — save will still proceed');
  }

  return {
    ...data,
    lastPromotedAt: new Date().toISOString(),
    // One-shot: clear the trigger so re-saving doesn't re-fire.
    promoteToProd: false,
  };
};
