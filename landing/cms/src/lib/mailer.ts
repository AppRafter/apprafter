// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Thin nodemailer wrapper. Returns a transporter when SMTP_* env
// is configured, otherwise logs a warning and returns null so hooks
// can degrade gracefully in dev (signup persists, email is skipped).

import { type Transporter, createTransport } from 'nodemailer';

let cached: Transporter | null | undefined;

export function getMailer(): Transporter | null {
  if (cached !== undefined) return cached;

  const host = process.env.SMTP_HOST;
  if (!host) {
    console.warn('[mailer] SMTP_HOST is empty — email sends will be skipped (dev mode).');
    cached = null;
    return null;
  }

  cached = createTransport({
    host,
    port: Number(process.env.SMTP_PORT ?? 587),
    secure: Number(process.env.SMTP_PORT ?? 587) === 465,
    auth:
      process.env.SMTP_USER && process.env.SMTP_PASS
        ? { user: process.env.SMTP_USER, pass: process.env.SMTP_PASS }
        : undefined,
  });
  return cached;
}

export const MAIL_FROM = process.env.SMTP_FROM ?? 'noreply@apprafter.dev';
