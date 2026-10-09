// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, jest, test } from 'bun:test';
import { act, renderHook } from '@testing-library/react';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { JsonValue } from '../ipc/generated/serde_json/JsonValue';
import type { UiError } from '../ipc/generated/UiError';
import { authRefusal, retryLine, useRetryCountdown } from './auth';

const refused = (code: string, fields: Record<string, JsonValue> = {}): UiError => ({
  code,
  message: `Rust says ${code}`,
  help: null,
  causes: [],
  fields,
});

describe('the back-off countdown', () => {
  test('says how many seconds are left', () => {
    expect(retryLine(30)).toBe('Too many failed attempts. Try again in 30 s.');
    expect(retryLine(1)).toBe('Too many failed attempts. Try again in 1 s.');
  });

  test("counts down Rust's retryInMs by the clock, never showing 0 while it holds", () => {
    jest.useFakeTimers();
    try {
      const { result } = renderHook(() => useRetryCountdown());
      expect(result.current[0]).toBeNull();
      act(() => result.current[1](2_500));
      expect(result.current[0]).toBe(3);
      act(() => jest.advanceTimersByTime(499));
      expect(result.current[0]).toBe(3);
      act(() => jest.advanceTimersByTime(1));
      expect(result.current[0]).toBe(2);
      act(() => jest.advanceTimersByTime(1_000));
      expect(result.current[0]).toBe(1);
      act(() => jest.advanceTimersByTime(999));
      expect(result.current[0]).toBe(1);
      act(() => jest.advanceTimersByTime(1));
      expect(result.current[0]).toBeNull();
    } finally {
      jest.useRealTimers();
    }
  });

  test('nothing to wait for holds nothing', () => {
    const { result } = renderHook(() => useRetryCountdown());
    act(() => result.current[1](0));
    expect(result.current[0]).toBeNull();
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
      retryInMs: null,
    });
  });

  test('…and plainly that the password is not right when it said nothing', () => {
    for (const fields of [{ exhausted: false }, { exhausted: false, messages: [] }, {}]) {
      expect(authRefusal(refused(DESKTOP_ERROR_CODES.AUTH_FAILED, fields), true)).toEqual({
        lines: ['That password is not right.'],
        retryInMs: null,
      });
    }
  });

  test("exhausted by Rust's back-off: what the OS said, and how long the back-off holds", () => {
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_FAILED, {
          exhausted: true,
          retryInMs: 30_000,
          messages: ['Authentication failure'],
        }),
        true,
      ),
    ).toEqual({ lines: ['Authentication failure'], retryInMs: 30_000 });
    // Turned away by the back-off itself: the password was never checked, so nothing says it
    // was wrong; the countdown says the rest.
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true, retryInMs: 12_345 }),
        true,
      ),
    ).toEqual({ lines: [], retryInMs: 12_345 });
  });

  test('exhausted with no end said (the OS’s own limit): try again later, nothing held', () => {
    for (const messages of [[], ['Maximum number of tries exceeded']]) {
      expect(
        authRefusal(refused(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true, messages }), true),
      ).toEqual({
        lines: [...messages, 'Too many failed attempts. The system will let you try again later.'],
        retryInMs: null,
      });
    }
  });

  test('a retryInMs that is not a positive number holds nothing', () => {
    for (const retryInMs of [0, -5, null, '30000', Number.NaN]) {
      expect(
        authRefusal(
          refused(DESKTOP_ERROR_CODES.AUTH_FAILED, {
            exhausted: true,
            retryInMs: retryInMs as JsonValue,
          }),
          true,
        ).retryInMs,
      ).toBeNull();
    }
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
      retryInMs: null,
    });
  });

  test("the OS's own lockout through its prompt (Hello, an account locked out): try later", () => {
    // Not "authentication failed": the account is locked, and saying less would hide it.
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_FAILED, {
          exhausted: true,
          // A prompt's refusal carries no words of the OS's; were there any, they stay unsaid.
          messages: ['not for the prompt'],
        }),
        false,
      ),
    ).toEqual({
      lines: ['Too many failed attempts. The system will let you try again later.'],
      retryInMs: null,
    });
  });

  test("the OS's own dialog turned away by Rust's back-off (Windows): only the countdown", () => {
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: true, retryInMs: 9_000 }),
        false,
      ),
    ).toEqual({ lines: [], retryInMs: 9_000 });
  });

  test('no agent: the system could not prompt; the field where it prompts: it does, try again', () => {
    expect(
      authRefusal(refused(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'no_agent' }), false)
        .lines,
    ).toEqual(['The system could not show its password prompt.']);
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'use_system_prompt' }),
        true,
      ).lines,
    ).toEqual(['The system asks for your password itself now. Try again.']);
  });

  test.each([true, false])(
    "the OS's own refusal is final, and never says to try again (field: %p)",
    (viaField) => {
      // An administrator's polkit rule, a Windows account outside its logon hours.
      const refusal = authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'not_permitted_here' }),
        viaField,
      );
      expect(refusal).toEqual({
        lines: ["This computer's settings do not allow AppRafter to ask for your password here."],
        retryInMs: null,
      });
      expect(refusal.lines.join(' ')).not.toMatch(/try again/i);
    },
  );

  test("the OS's own refusal with its own words (the field's route): those, then the line", () => {
    expect(
      authRefusal(
        refused(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, {
          reason: 'not_permitted_here',
          messages: ['Your account is not allowed to log in at this time'],
        }),
        true,
      ).lines,
    ).toEqual([
      'Your account is not allowed to log in at this time',
      "This computer's settings do not allow AppRafter to ask for your password here.",
    ]);
  });

  test("anything else is Rust's own message: a prompt's failure, a cancel, another reason", () => {
    for (const error of [
      refused(DESKTOP_ERROR_CODES.AUTH_FAILED, { exhausted: false }),
      refused(DESKTOP_ERROR_CODES.AUTH_FAILED),
      refused(DESKTOP_ERROR_CODES.AUTH_CANCELLED),
      refused(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE, { reason: 'policy_missing' }),
      refused(DESKTOP_ERROR_CODES.INTERNAL),
    ]) {
      expect(authRefusal(error, false)).toEqual({ lines: [error.message], retryInMs: null });
    }
  });
});
