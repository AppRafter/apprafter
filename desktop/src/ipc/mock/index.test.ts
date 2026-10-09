// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, describe, expect, test } from 'bun:test';
import { Channel } from '@tauri-apps/api/core';
import { clearMocks } from '@tauri-apps/api/mocks';
import * as api from '../api';
import { IpcError } from '../api';
import { onLockChanged } from '../events';
import { ALLOWED_WHILE_LOCKED, type COMMANDS } from '../generated/commands';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import type { LockState } from '../generated/LockState';
import type { OpEvent } from '../generated/OpEvent';
import { installMockIpc, mockOptionsFromUrl } from './index';

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
      if (name === 'unlock') continue;
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
    const unlocked = await api.unlock();
    expect(unlocked).toMatchObject({ locked: false, reason: null });
    const locked = await api.lockNow();
    expect(locked).toMatchObject({ locked: true, reason: 'manual' });
    await settle();
    expect(heard.map((s) => s.locked)).toEqual([false, true]);
    unlisten();
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
    expect((await api.settingsGet()).theme).toBe('light');
  });

  test('an unknown command is refused as Tauri refuses it, not answered', async () => {
    installMockIpc();
    const { invoke } = await import('@tauri-apps/api/core');
    await expect(invoke('target_list')).rejects.toContain('target_list');
  });
});

describe('mockOptionsFromUrl', () => {
  test('reads os and theme', () => {
    expect(mockOptionsFromUrl('?os=macos&theme=system')).toEqual({ os: 'macos', theme: 'system' });
    expect(mockOptionsFromUrl('')).toEqual({});
  });

  test('refuses a value it does not know', () => {
    expect(() => mockOptionsFromUrl('?os=beos')).toThrow('os');
    expect(() => mockOptionsFromUrl('?theme=sepia')).toThrow('theme');
  });
});
