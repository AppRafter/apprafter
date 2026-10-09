// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// How the UI names the OS mechanism that confirms the device owner (AppInfo.auth.method):
// "Confirm with Windows Hello next.", "Ask for your account password before…"; and what it says
// when a check is refused, on the lock screen and in the confirm dialog alike.
import { useCallback, useEffect, useState } from 'react';
import type { AuthMethod } from '../ipc/generated/AuthMethod';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { UiError } from '../ipc/generated/UiError';

const PROMPTS: Record<AuthMethod, string> = {
  windows_hello: 'Windows Hello',
  windows_credential: 'your Windows password',
  mac_local_authentication: 'Touch ID or your Mac password',
  polkit: 'your account password',
  pam: 'your account password',
  fake: 'the test build prompt',
};

export function authPrompt(method: AuthMethod): string {
  return PROMPTS[method];
}

/**
 * How long Rust turns password checks away after too many failures (`Backoff::REFUSAL_MS`,
 * desktop/os-auth/src/outcome.rs; auth.test.ts holds the two equal). Rust does not say how much
 * of it is left, so the field waits this long from the answer that said `exhausted`: never less
 * than Rust's refusal still runs.
 */
export const BACKOFF_MS = 30_000;

/** What the password field says while it waits out the back-off. */
export const BACKOFF_LINE = 'Too many failed attempts. Wait 30 seconds, then try again.';

const WRONG_PASSWORD = 'That password is not right.';

const BUSY = 'A check is already open. Finish it, then try again.';

/** The `auth_unavailable` reasons that come with a way on, in plain words. */
const UNAVAILABLE: Partial<Record<string, string>> = {
  // Linux: polkit has no agent to show its dialog; the app's own field takes over.
  no_agent: 'The system could not show its password prompt.',
  // The field was used where the OS prompts itself (a stale field, or a lock in between).
  not_permitted_here: 'The system asks for your password itself now. Try again.',
};

export interface AuthRefusal {
  /** What to say, a line each: what the OS said, or the app's own words. */
  readonly lines: readonly string[];
  /** Too many failed attempts: the field waits BACKOFF_MS, saying BACKOFF_LINE meanwhile. */
  readonly backoff: boolean;
}

/** What the OS said during a check of the app's own field (PAM's messages), never the password. */
function messagesOf(error: UiError): string[] {
  const messages = error.fields.messages;
  if (!Array.isArray(messages)) return [];
  return messages.filter(
    (message): message is string => typeof message === 'string' && message.trim() !== '',
  );
}

/**
 * What a refused unlock or confirmation says. `viaField`: the password came from the app's own
 * field, so a failure is about that password — the OS's own words when it gave any — and
 * `exhausted` starts the back-off. Through the OS's prompt, a failure is shown as Rust words it.
 */
export function authRefusal(error: UiError, viaField: boolean): AuthRefusal {
  if (error.code === DESKTOP_ERROR_CODES.AUTH_FAILED && viaField) {
    const exhausted = error.fields.exhausted === true;
    const said = messagesOf(error);
    // Turned away by the back-off itself, the password was not checked: nothing says it was
    // wrong.
    return { lines: said.length > 0 || exhausted ? said : [WRONG_PASSWORD], backoff: exhausted };
  }
  if (error.code === DESKTOP_ERROR_CODES.AUTH_BUSY) return { lines: [BUSY], backoff: false };
  if (error.code === DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE) {
    const line = UNAVAILABLE[String(error.fields.reason)];
    if (line !== undefined) return { lines: [line], backoff: false };
  }
  return { lines: [error.message], backoff: false };
}

/** A back-off: `[waiting, start]`; `start()` holds `waiting` true for `ms`. */
export function useBackoff(ms: number = BACKOFF_MS): readonly [boolean, () => void] {
  const [waiting, setWaiting] = useState(false);
  useEffect(() => {
    if (!waiting) return;
    const timer = setTimeout(() => setWaiting(false), ms);
    return () => clearTimeout(timer);
  }, [waiting, ms]);
  const start = useCallback(() => setWaiting(true), []);
  return [waiting, start];
}
