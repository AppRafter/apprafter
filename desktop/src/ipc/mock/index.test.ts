// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, describe, expect, jest, test } from 'bun:test';
import { Channel } from '@tauri-apps/api/core';
import { clearMocks } from '@tauri-apps/api/mocks';
import * as api from '../api';
import { IpcError } from '../api';
import { onLockChanged } from '../events';
import { ALLOWED_WHILE_LOCKED, type COMMANDS } from '../generated/commands';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import type { LockState } from '../generated/LockState';
import type { OpEvent } from '../generated/OpEvent';
import {
  installMockIpc,
  MOCK_BACKOFF,
  MOCK_PAM_SAYS,
  MOCK_PASSWORD,
  mockOptionsFromUrl,
} from './index';

afterEach(() => clearMocks());

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Every command, called the way the app calls it. */
function callEach(): Record<(typeof COMMANDS)[number], () => Promise<unknown>> {
  const channel = new Channel<OpEvent>();
  return {
    activity: () => api.activity(),
    app_info: () => api.appInfo(),
    lock_now: () => api.lockNow(),
    lock_status: () => api.lockStatus(),
    op_cancel: () => api.opCancel(1),
    op_discard: () => api.opDiscard(1),
    op_execute: () => api.opExecute(1, channel),
    op_list: () => api.opList(),
    op_subscribe: () => api.opSubscribe(1, channel),
    op_unsubscribe: () => api.opUnsubscribe(1, 1),
    quit: () => api.quit(),
    settings_get: () => api.settingsGet(),
    settings_set: async () => api.settingsSet(await api.settingsGet()),
    unlock: () => api.unlock(),
    unlock_with_password: () => api.unlockWithPassword(MOCK_PASSWORD),
    window_ready: () => api.windowReady(),
  };
}

/** The UiError code a call rejected with, or 'answered'. */
async function outcome(call: () => Promise<unknown>): Promise<string | null> {
  try {
    await call();
    return 'answered';
  } catch (e) {
    if (!(e instanceof IpcError)) throw e;
    return e.error.code;
  }
}

describe('installMockIpc', () => {
  test('starts locked at startup, and answers only what Rust answers while locked', async () => {
    installMockIpc();
    const state = await api.lockStatus();
    expect(state).toMatchObject({ locked: true, reason: 'startup', autoLockMinutes: 10 });
    for (const [name, call] of Object.entries(callEach())) {
      if (name === 'unlock' || name === 'unlock_with_password') continue;
      const allowed = (ALLOWED_WHILE_LOCKED as readonly string[]).includes(name);
      const code = await outcome(call);
      if (allowed) expect(code, name).not.toBe(DESKTOP_ERROR_CODES.LOCKED);
      else expect(code, name).toBe(DESKTOP_ERROR_CODES.LOCKED);
    }
  });

  test('unlocked, every command has an answer (an unknown operation is plan_not_found)', async () => {
    installMockIpc();
    await api.unlock();
    for (const [name, call] of Object.entries(callEach())) {
      // It would lock the app for the calls after it; the lock-changed test covers it.
      if (name === 'lock_now') continue;
      const fine: (string | null)[] = ['answered', DESKTOP_ERROR_CODES.PLAN_NOT_FOUND];
      expect(fine, name).toContain(await outcome(call));
    }
  });

  test('unlock and lock now emit lock-changed with the new state', async () => {
    installMockIpc();
    const heard: LockState[] = [];
    const unlisten = await onLockChanged((state) => heard.push(state));
    expect((await api.lockStatus()).seq).toBe(0);
    const unlocked = await api.unlock();
    expect(unlocked).toMatchObject({ locked: false, reason: null, seq: 1 });
    const locked = await api.lockNow();
    expect(locked).toMatchObject({ locked: true, reason: 'manual', seq: 2 });
    await settle();
    expect(heard.map((s) => [s.locked, s.seq])).toEqual([
      [false, 1],
      [true, 2],
    ]);
    unlisten();
  });

  test('on the PAM route the field unlocks with the demo password and refuses others as PAM would', async () => {
    installMockIpc({ auth: 'pam' });
    expect((await api.appInfo()).auth).toMatchObject({ method: 'pam', passwordField: true });
    const heard: LockState[] = [];
    const unlisten = await onLockChanged((state) => heard.push(state));
    const refused = await api.unlockWithPassword('guess').catch((e: unknown) => e);
    expect(refused).toBeInstanceOf(IpcError);
    expect((refused as IpcError).error).toMatchObject({
      code: DESKTOP_ERROR_CODES.AUTH_FAILED,
      fields: { exhausted: false, messages: [MOCK_PAM_SAYS] },
    });
    expect((await api.lockStatus()).locked).toBe(true);
    expect(await api.unlockWithPassword(MOCK_PASSWORD)).toMatchObject({ locked: false, seq: 1 });
    // Unlocked, nothing is checked.
    expect(await api.unlockWithPassword('guess')).toMatchObject({ locked: false, seq: 1 });
    await settle();
    expect(heard.map((s) => s.locked)).toEqual([false]);
    unlisten();
  });

  test("on the PAM route too many wrong passwords start Rust's back-off, saying how long it holds", async () => {
    jest.useFakeTimers();
    try {
      installMockIpc({ auth: 'pam' });
      const refusal = async (password: string) => {
        const refused = await api.unlockWithPassword(password).catch((e: unknown) => e);
        expect(refused).toBeInstanceOf(IpcError);
        return (refused as IpcError).error;
      };
      for (let i = 1; i < MOCK_BACKOFF.failures; i += 1) {
        expect((await refusal('guess')).fields).toEqual({
          exhausted: false,
          messages: [MOCK_PAM_SAYS],
        });
      }
      // The failure that starts it says how long it lasts, with what PAM said.
      expect(await refusal('guess')).toMatchObject({
        code: DESKTOP_ERROR_CODES.AUTH_FAILED,
        fields: { exhausted: true, retryInMs: MOCK_BACKOFF.refusalMs, messages: [MOCK_PAM_SAYS] },
      });
      // While it holds, even the right password is turned away unchecked, with what is left.
      jest.advanceTimersByTime(10_000);
      expect((await refusal(MOCK_PASSWORD)).fields).toEqual({
        exhausted: true,
        retryInMs: MOCK_BACKOFF.refusalMs - 10_000,
      });
      jest.advanceTimersByTime(MOCK_BACKOFF.refusalMs - 10_000);
      expect(await api.unlockWithPassword(MOCK_PASSWORD)).toMatchObject({ locked: false });
    } finally {
      jest.useRealTimers();
    }
  });

  test("the mock's back-off is Rust's: as many failures, as long a refusal", async () => {
    const outcomeRs = await Bun.file(
      new URL('../../../os-auth/src/outcome.rs', import.meta.url),
    ).text();
    const constant = (name: string, type: string) => {
      const value = outcomeRs.match(new RegExp(`pub const ${name}: ${type} = ([\\d_]+);`))?.[1];
      expect(value, `Backoff::${name} in os-auth/src/outcome.rs`).toBeDefined();
      return Number(value?.replaceAll('_', ''));
    };
    expect(MOCK_BACKOFF).toEqual({
      failures: constant('FAILURES', 'u32'),
      refusalMs: constant('REFUSAL_MS', 'u64'),
    });
  });

  test('a new idle time is numbered and heard; a save that changes nothing is not', async () => {
    installMockIpc();
    await api.unlock();
    const heard: LockState[] = [];
    const unlisten = await onLockChanged((state) => heard.push(state));
    const settings = await api.settingsGet();
    await api.settingsSet({ ...settings, theme: 'light' });
    await api.settingsSet({ ...settings, autoLock: '5' });
    await settle();
    expect(heard).toEqual([await api.lockStatus()]);
    expect(heard[0]).toMatchObject({ locked: false, autoLockMinutes: 5, seq: 2 });
    unlisten();
  });

  test('on the PAM route a prompt finds no agent; where the OS prompts, the field is refused', async () => {
    installMockIpc({ auth: 'pam', os: 'linux' });
    expect(await outcome(() => api.unlock())).toBe(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE);
    const noAgent = await api.unlock().catch((e: unknown) => e);
    expect((noAgent as IpcError).error.fields).toEqual({ reason: 'no_agent' });
    expect((await api.lockStatus()).locked).toBe(true);
    clearMocks();

    installMockIpc({ os: 'linux' });
    expect((await api.appInfo()).auth.passwordField).toBe(false);
    const refused = await api.unlockWithPassword(MOCK_PASSWORD).catch((e: unknown) => e);
    expect((refused as IpcError).error).toMatchObject({
      code: DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
      fields: { reason: 'use_system_prompt' },
    });
    expect((await api.lockStatus()).locked).toBe(true);
    expect(await api.unlock()).toMatchObject({ locked: false });
  });

  test('with the lock switched off, lock now leaves the app unlocked', async () => {
    installMockIpc();
    await api.unlock();
    const settings = await api.settingsGet();
    await api.settingsSet({ ...settings, lockEnabled: false });
    expect(await api.lockNow()).toMatchObject({ locked: false, autoLockMinutes: null });
  });

  test('the OS and the theme come from the options', async () => {
    installMockIpc({ os: 'windows', theme: 'light' });
    expect((await api.appInfo()).os).toBe('windows');
    expect((await api.appInfo()).sessionEvents).toEqual({ lock: true, sleep: true });
    expect((await api.settingsGet()).theme).toBe('light');
  });

  test('what the session tells comes from the options: both, one half, or nothing', async () => {
    const cases = [
      ['both', { lock: true, sleep: true }],
      ['lock', { lock: true, sleep: false }],
      ['sleep', { lock: false, sleep: true }],
      ['none', { lock: false, sleep: false }],
    ] as const;
    for (const [session, events] of cases) {
      installMockIpc({ session });
      expect((await api.appInfo()).sessionEvents, session).toEqual(events);
      clearMocks();
    }
  });

  test('an unknown command is refused as Tauri refuses it, not answered', async () => {
    installMockIpc();
    const { invoke } = await import('@tauri-apps/api/core');
    await expect(invoke('target_list')).rejects.toContain('target_list');
  });
});

describe('mockOptionsFromUrl', () => {
  test('reads os, theme, auth and session', () => {
    expect(mockOptionsFromUrl('?os=macos&theme=system')).toEqual({ os: 'macos', theme: 'system' });
    expect(mockOptionsFromUrl('?auth=pam')).toEqual({ auth: 'pam' });
    expect(mockOptionsFromUrl('?session=none')).toEqual({ session: 'none' });
    expect(mockOptionsFromUrl('')).toEqual({});
  });

  test('refuses a value it does not know', () => {
    expect(() => mockOptionsFromUrl('?os=beos')).toThrow('os');
    expect(() => mockOptionsFromUrl('?theme=sepia')).toThrow('theme');
    expect(() => mockOptionsFromUrl('?auth=fingerprint')).toThrow('auth');
    expect(() => mockOptionsFromUrl('?session=hibernate')).toThrow('session');
  });
});
