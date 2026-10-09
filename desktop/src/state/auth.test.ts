// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { JsonValue } from '../ipc/generated/serde_json/JsonValue';
import type { UiError } from '../ipc/generated/UiError';
import { authRefusal, BACKOFF_LINE, BACKOFF_MS } from './auth';

const refused = (code: string, fields: Record<string, JsonValue> = {}): UiError => ({
  code,
  message: `Rust says ${code}`,
  help: null,
  causes: [],
  fields,
});

const outcomeRs = await Bun.file(new URL('../../os-auth/src/outcome.rs', import.meta.url)).text();

describe('the back-off', () => {
  test("lasts as long as Rust's refusal after too many failures, and says so", () => {
    const refusal = outcomeRs.match(/pub const REFUSAL_MS: u64 = ([\d_]+);/)?.[1];
    expect(refusal, 'Backoff::REFUSAL_MS in os-auth/src/outcome.rs').toBeDefined();
    expect(Number(refusal?.replaceAll('_', ''))).toBe(BACKOFF_MS);
    expect(BACKOFF_LINE).toContain(`${BACKOFF_MS / 1000} seconds`);
  });
});

describe('authRefusal, for the password field', () => {
  test('a wrong password says what the OS said, a line each, when it said anything', () => {
    const refusal = authRefusal(
      refused(DESKTOP_ERROR_CODES.AUTH_FAILED, {
        exhausted: false,
        messages: ['Authentication failure', 'Place your finger on the reader'],
      }),
      true,
    );
    expect(refusal).toEqual({
      lines: ['Authentication failure', 'Place your finger on the reader'],
      backoff: false,
    });
  });

  test('…and plainly that the password is not right when it said nothing', () => {
    for (const fields of [{ exhausted: false }, { exhausted: false, messages: [] }, {}]) {
      expect(authRefusal(refused(DESKTOP_ERROR_CODES.AUTH_FAILED, fields), true)).toEqual({
        lines: ['That password is not right.'],
        backoff: false,
      });
    }
  });

  test('exhausted: the back-off, after what the OS said; without its words, only the back-off', () => {
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_FAILED, {
          exhausted: true,
          messages: ['Authentication failure'],
        }),
        true,
      ),
    ).toEqual({ lines: ['Authentication failure'], backoff: true });
    // Turned away by the back-off itself: the password was never checked, so nothing says it
    // was wrong.
    expect(
      authRefusal(refused(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true }), true),
    ).toEqual({ lines: [], backoff: true });
  });

  test('what is not a string in the messages is left out', () => {
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_FAILED, { messages: ['said', 7, null, '  '] }),
        true,
      ).lines,
    ).toEqual(['said']);
  });
});

describe('authRefusal, for either way', () => {
  test.each([true, false])('busy says a check is already open (field: %p)', (viaField) => {
    expect(authRefusal(refused(DESKTOP_ERROR_CODES.AUTH_BUSY), viaField)).toEqual({
      lines: ['A check is already open. Finish it, then try again.'],
      backoff: false,
    });
  });

  test('no agent: the system could not prompt; not here: it prompts itself', () => {
    expect(
      authRefusal(refused(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'no_agent' }), false)
        .lines,
    ).toEqual(['The system could not show its password prompt.']);
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'not_permitted_here' }),
        true,
      ).lines,
    ).toEqual(['The system asks for your password itself now. Try again.']);
  });

  test("anything else is Rust's own message: a prompt's failure, a cancel, another reason", () => {
    for (const error of [
      refused(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true }),
      refused(DESKTOP_ERROR_CODES.AUTH_CANCELLED),
      refused(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'policy_missing' }),
      refused(DESKTOP_ERROR_CODES.INTERNAL),
    ]) {
      expect(authRefusal(error, false)).toEqual({ lines: [error.message], backoff: false });
    }
  });
});
