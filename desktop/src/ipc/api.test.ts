// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Channel } from '@tauri-apps/api/core';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import * as api from './api';
import { COMMANDS } from './generated/commands';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { OpEvent } from './generated/OpEvent';
import type { Settings } from './generated/Settings';
import type { UiError } from './generated/UiError';

const SETTINGS: Settings = {
  version: 1,
  theme: 'dark',
  lockEnabled: true,
  lockOnStart: true,
  lockOnSleep: true,
  hello: true,
  autoLock: '10',
  refresh: '5',
  pauseHidden: true,
  osNotify: true,
  trayBadge: true,
  closeToTray: true,
};

let calls: { cmd: string; args: unknown }[];
let answer: (cmd: string) => unknown;

beforeEach(() => {
  calls = [];
  answer = () => null;
  mockIPC((cmd, args) => {
    calls.push({ cmd, args });
    return answer(cmd);
  });
});

afterEach(() => clearMocks());

test('API_COMMANDS is exactly the commands Rust registers', () => {
  expect(new Set(api.API_COMMANDS).size).toBe(api.API_COMMANDS.length);
  expect([...api.API_COMMANDS].sort()).toEqual([...COMMANDS].sort());
});

describe('each function sends its command with camelCase arguments', () => {
  // Built after beforeEach installed the mock: a Channel registers its callback through it.
  const cases = (
    channel: Channel<OpEvent>,
  ): Record<string, [() => Promise<unknown>, string, unknown]> => ({
    appInfo: [() => api.appInfo(), 'app_info', {}],
    settingsGet: [() => api.settingsGet(), 'settings_get', {}],
    settingsSet: [() => api.settingsSet(SETTINGS), 'settings_set', { settings: SETTINGS }],
    lockStatus: [() => api.lockStatus(), 'lock_status', {}],
    lockNow: [() => api.lockNow(), 'lock_now', {}],
    unlock: [() => api.unlock(), 'unlock', {}],
    activity: [() => api.activity(), 'activity', {}],
    quit: [() => api.quit(), 'quit', {}],
    opList: [() => api.opList(), 'op_list', {}],
    opSubscribe: [() => api.opSubscribe(7, channel), 'op_subscribe', { opId: 7, onEvent: channel }],
    opUnsubscribe: [() => api.opUnsubscribe(7, 3), 'op_unsubscribe', { opId: 7, subscription: 3 }],
    opCancel: [() => api.opCancel(7), 'op_cancel', { opId: 7 }],
    opDiscard: [() => api.opDiscard(7), 'op_discard', { opId: 7 }],
    opExecute: [() => api.opExecute(7, channel), 'op_execute', { opId: 7, onEvent: channel }],
  });

  test('every exported function is covered, and together they call every command', async () => {
    const table = cases(new Channel<OpEvent>());
    const functions = Object.entries(api)
      .filter(([, value]) => typeof value === 'function' && value !== api.IpcError)
      .map(([name]) => name);
    expect(functions.sort()).toEqual(Object.keys(table).sort());
    for (const [run] of Object.values(table)) await run();
    expect(new Set(calls.map((c) => c.cmd))).toEqual(new Set(api.API_COMMANDS));
  });

  test('with the arguments Rust reads', async () => {
    for (const [name, [run, cmd, args]] of Object.entries(cases(new Channel<OpEvent>()))) {
      calls = [];
      await run();
      expect(calls, name).toEqual([{ cmd, args }]);
    }
  });
});

test('an answer passes through', async () => {
  answer = (cmd) => (cmd === 'settings_get' ? SETTINGS : null);
  expect(await api.settingsGet()).toEqual(SETTINGS);
});

describe('a rejection is an IpcError', () => {
  test('carrying the UiError the command answered with', async () => {
    const locked: UiError = {
      code: DESKTOP_ERROR_CODES.LOCKED,
      message: 'AppRafter is locked.',
      help: null,
      causes: [],
      fields: {},
    };
    answer = () => Promise.reject(locked);
    const error = await api.opList().catch((e: unknown) => e);
    expect(error).toBeInstanceOf(api.IpcError);
    expect(error).toMatchObject({
      command: 'op_list',
      error: locked,
      message: 'AppRafter is locked.',
    });
  });

  test("or Tauri's own refusal (a string), with no code", async () => {
    answer = () =>
      Promise.reject('op_cancel not allowed. Permissions associated with this command');
    const error = await api.opCancel(1).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(api.IpcError);
    expect((error as api.IpcError).error).toEqual({
      code: null,
      message: 'op_cancel not allowed. Permissions associated with this command',
      help: null,
      causes: [],
      fields: {},
    });
  });
});
