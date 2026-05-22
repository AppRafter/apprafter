// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import type { CollectionAfterChangeHook } from 'payload';

import { MAIL_FROM, getMailer } from '../lib/mailer';

type WaitlistDoc = {
  id: string | number;
  email: string;
  wantsCall?: boolean;
  callEmailSentAt?: string | null;
};

/**
 * Fires after every WaitlistSignups create/update. Sends the
 * discovery-call email exactly once per signup — the
 * callEmailSentAt timestamp is the idempotency guard. Failures
 * are logged but do not throw; a signup must persist even when
 * SMTP is misconfigured (the hook surfaces a warning so the
 * operator notices, dev keeps moving).
 */
export const sendDiscoveryEmail: CollectionAfterChangeHook<WaitlistDoc> = async ({
  doc,
  req,
  operation,
}) => {
  if (operation !== 'create' && operation !== 'update') return doc;
  if (!doc.wantsCall) return doc;
  if (doc.callEmailSentAt) return doc;

  const mailer = getMailer();
  if (!mailer) {
    req.payload.logger.warn(
      `[sendDiscoveryEmail] SMTP not configured — skipping send for ${doc.email}`,
    );
    return doc;
  }

  let booking: {
    discoveryCallUrl: string;
    discoveryCallEmailSubject: string;
    discoveryCallEmailBody: string;
  };
  try {
    booking = (await req.payload.findGlobal({ slug: 'booking' })) as typeof booking;
  } catch (err) {
    req.payload.logger.error({ err }, '[sendDiscoveryEmail] failed to load Booking global');
    return doc;
  }

  const body = booking.discoveryCallEmailBody.replace(/\{\{url\}\}/g, booking.discoveryCallUrl);

  try {
    await mailer.sendMail({
      from: MAIL_FROM,
      to: doc.email,
      subject: booking.discoveryCallEmailSubject,
      text: body,
    });
    await req.payload.update({
      collection: 'waitlist-signups',
      id: doc.id,
      data: { callEmailSentAt: new Date().toISOString() },
      // Avoid recursion into this very hook.
      depth: 0,
      overrideAccess: true,
    });
    req.payload.logger.info(`[sendDiscoveryEmail] Calendly link sent to ${doc.email}`);
  } catch (err) {
    req.payload.logger.error({ err }, `[sendDiscoveryEmail] send failed for ${doc.email}`);
  }

  return doc;
};
