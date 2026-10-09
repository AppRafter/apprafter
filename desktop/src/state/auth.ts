// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// How the UI names the OS mechanism that confirms the device owner (AppInfo.auth.method):
// "Confirm with Windows Hello next.", "Ask for your account password before…"; what it says
// when a check is refused, on the lock screen and in the confirm dialog alike; and the countdown
// of Rust's back-off after too many failures, from how long Rust says it still refuses.
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

/** What a refusal says while Rust's back-off holds, `seconds` (rounded up) before it ends. */
export function retryLine(seconds: number): string {
  return `Too many failed attempts. Try again in ${seconds} s.`;
}

/**
 * Too many attempts, and the OS's own limit holds — Windows Hello's, an account Windows locked
 * out, Touch ID's, PAM's — whose end Rust is not told.
 */
const TRY_LATER = 'Too many failed attempts. The system will let you try again later.';

const WRONG_PASSWORD = 'That password is not right.';

const BUSY = 'A check is already open. Finish it, then try again.';

/** The `auth_unavailable` reasons that come with a way on, in plain words. */
const UNAVAILABLE: Partial<Record<string, string>> = {
  // Linux: polkit has no agent to show its dialog; the app's own field takes over.
  no_agent: 'The system could not show its password prompt.',
  // The field was used where the OS prompts itself (a stale field, or a lock in between): the
  // re-read app_info then takes the field away, and the OS's prompt is the way.
  use_system_prompt: 'The system asks for your password itself now. Try again.',
  // Windows: the right password, expired or one that must change at the next sign-in. Final for
  // this attempt: nothing is held, and the owner changes it in the system first.
  password_expired: 'Your system password has expired. Change it, then try again.',
};

/**
 * `not_permitted_here`: the OS refuses here for good as things stand — an administrator's polkit
 * rule, a Windows account outside its logon hours or with a password that must change. Final,
 * so it never says to try again.
 */
const NOT_PERMITTED =
  "This computer's settings do not allow AppRafter to ask for your password here.";

export interface AuthRefusal {
  /** What to say, a line each: what the OS said, or the app's own words. */
  readonly lines: readonly string[];
  /**
   * Rust's back-off refuses every attempt for this long (`fields.retryInMs`): the page holds
   * the field or the button and counts it down with retryLine. `null` when nothing is held.
   */
  readonly retryInMs: number | null;
}

/** What the OS said during a check of the app's own field (PAM's messages), never the password. */
function messagesOf(error: UiError): string[] {
  const messages = error.fields.messages;
  if (!Array.isArray(messages)) return [];
  return messages.filter(
    (message): message is string => typeof message === 'string' && message.trim() !== '',
  );
}

/** How long Rust's back-off still refuses (`fields.retryInMs`), when it says so. */
function retryInMsOf(error: UiError): number | null {
  const ms = error.fields.retryInMs;
  return typeof ms === 'number' && Number.isFinite(ms) && ms > 0 ? ms : null;
}

/**
 * What a refused unlock or confirmation says. `viaField`: the password came from the app's own
 * field, so a failure is about that password — the OS's own words when it gave any. Too many
 * failures, whichever way the owner was asked: when Rust's back-off holds, how long it does
 * (`retryInMs`, counted down by the page; the Windows credential dialog has the back-off too);
 * when the OS's own limit holds, that the system lets the owner try later — never only
 * "authentication failed", which would hide a locked account. Any other failure through the
 * OS's prompt is shown as Rust words it.
 */
export function authRefusal(error: UiError, viaField: boolean): AuthRefusal {
  if (error.code === DESKTOP_ERROR_CODES.AUTH_FAILED) {
    const retryInMs = retryInMsOf(error);
    const exhausted = error.fields.exhausted === true;
    const said = viaField ? messagesOf(error) : [];
    // Turned away by the back-off itself, the password was not checked: nothing says it was
    // wrong, and the countdown says the rest.
    if (retryInMs !== null) return { lines: said, retryInMs };
    if (exhausted) return { lines: [...said, TRY_LATER], retryInMs: null };
    if (viaField) {
      return { lines: said.length > 0 ? said : [WRONG_PASSWORD], retryInMs: null };
    }
  }
  if (error.code === DESKTOP_ERROR_CODES.AUTH_BUSY) return { lines: [BUSY], retryInMs: null };
  if (error.code === DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE) {
    const reason = String(error.fields.reason);
    // What the OS said of it (the field's route) comes first: the more specific words.
    if (reason === 'not_permitted_here') {
      return { lines: [...(viaField ? messagesOf(error) : []), NOT_PERMITTED], retryInMs: null };
    }
    const line = UNAVAILABLE[reason];
    if (line !== undefined) return { lines: [line], retryInMs: null };
  }
  return { lines: [error.message], retryInMs: null };
}

/**
 * The countdown of Rust's back-off: `[secondsLeft, start]`. `start(ms)` holds from now for `ms`
 * (a refusal's `retryInMs`), by the monotonic `performance.now()`; `secondsLeft` is the whole
 * seconds left, rounded up — never 0 while it holds — or `null` when nothing is held. It ticks
 * when the shown number changes. Rust measured `ms` before its answer crossed IPC, so this ends
 * a little after Rust's refusal does, never before.
 */
export function useRetryCountdown(): readonly [number | null, (ms: number) => void] {
  const [held, setHeld] = useState<{ until: number; left: number } | null>(null);
  const until = held?.until ?? null;
  useEffect(() => {
    if (until === null) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = () => {
      const left = until - performance.now();
      if (left <= 0) {
        setHeld(null);
        return;
      }
      setHeld({ until, left });
      // Wake when the rounded-up second changes.
      timer = setTimeout(tick, left % 1000 || 1000);
    };
    tick();
    return () => clearTimeout(timer);
  }, [until]);
  const start = useCallback((ms: number) => {
    setHeld(ms > 0 ? { until: performance.now() + ms, left: ms } : null);
  }, []);
  return [held === null ? null : Math.ceil(held.left / 1000), start];
}
