// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { Channel } from '@tauri-apps/api/core';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import * as api from './api';
import { COMMANDS } from './generated/commands';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { OpEvent } from './generated/OpEvent';
import type { Settings } from './generated/Settings';
import type { TargetAddArgs } from './generated/TargetAddArgs';
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

/** What the add wizard sends: the token by its draft, never the token. */
const ADD_ARGS: TargetAddArgs = {
  name: 'lab-2',
  provider: 'hetzner-cloud',
  draftId: 3,
  sshKey: null,
  region: 'nbg1',
  tier: 'solo',
  serverType: 'cx22',
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
    unlockWithPassword: [
      () => api.unlockWithPassword('open sesame'),
      'unlock_with_password',
      { password: 'open sesame' },
    ],
    activity: [() => api.activity(), 'activity', {}],
    quit: [() => api.quit(), 'quit', {}],
    opList: [() => api.opList(), 'op_list', {}],
    opSubscribe: [() => api.opSubscribe(7, channel), 'op_subscribe', { opId: 7, onEvent: channel }],
    opUnsubscribe: [() => api.opUnsubscribe(7, 3), 'op_unsubscribe', { opId: 7, subscription: 3 }],
    opCancel: [() => api.opCancel(7), 'op_cancel', { opId: 7 }],
    opDiscard: [() => api.opDiscard(7), 'op_discard', { opId: 7 }],
    opExecute: [() => api.opExecute(7, channel), 'op_execute', { opId: 7, onEvent: channel }],
    windowReady: [() => api.windowReady(), 'window_ready', {}],
    themeApply: [() => api.themeApply('system'), 'theme_apply', { theme: 'system' }],
    targetList: [() => api.targetList(), 'target_list', {}],
    targetShow: [() => api.targetShow('prod-eu'), 'target_show', { name: 'prod-eu' }],
    sshKeyCandidates: [() => api.sshKeyCandidates(), 'ssh_key_candidates', {}],
    sshKeyInspect: [
      () => api.sshKeyInspect('~/.ssh/id_ed25519.pub'),
      'ssh_key_inspect',
      { path: '~/.ssh/id_ed25519.pub' },
    ],
    toolchainStatus: [() => api.toolchainStatus(), 'toolchain_status', {}],
    whoami: [() => api.whoami(), 'whoami', {}],
    opStartVerifyToken: [
      () => api.opStartVerifyToken('hetzner-cloud', 'not-a-real-token'),
      'op_start_verify_token',
      { provider: 'hetzner-cloud', token: 'not-a-real-token' },
    ],
    opStartMachineCatalogue: [
      () => api.opStartMachineCatalogue({ kind: 'draft', draftId: 3 }),
      'op_start_machine_catalogue',
      { source: { kind: 'draft', draftId: 3 } },
    ],
    opStartRegionLatencies: [
      () => api.opStartRegionLatencies(['nbg1', 'hel1']),
      'op_start_region_latencies',
      { regions: ['nbg1', 'hel1'] },
    ],
    opStartDoctor: [() => api.opStartDoctor('prod-eu'), 'op_start_doctor', { target: 'prod-eu' }],
    opStartWhoami: [() => api.opStartWhoami(), 'op_start_whoami', {}],
    opPlanTargetAdd: [
      () => api.opPlanTargetAdd(ADD_ARGS),
      'op_plan_target_add',
      { args: ADD_ARGS },
    ],
    opPlanTargetRenew: [
      () => api.opPlanTargetRenew('prod-eu', 'not-a-real-token', '/home/alex/.ssh/work.pub'),
      'op_plan_target_renew',
      { name: 'prod-eu', token: 'not-a-real-token', sshKey: '/home/alex/.ssh/work.pub' },
    ],
    opPlanTargetUse: [
      () => api.opPlanTargetUse('prod-eu'),
      'op_plan_target_use',
      { name: 'prod-eu' },
    ],
    opPlanTargetRename: [
      () => api.opPlanTargetRename('prod-eu', 'prod-us'),
      'op_plan_target_rename',
      { from: 'prod-eu', to: 'prod-us' },
    ],
    opPlanTargetRemove: [
      () => api.opPlanTargetRemove('prod-eu'),
      'op_plan_target_remove',
      { name: 'prod-eu' },
    ],
    opPlanTargetMachine: [
      () => api.opPlanTargetMachine('lab', 'cx32', null),
      'op_plan_target_machine',
      { name: 'lab', sku: 'cx32', region: null },
    ],
    targetDraftDiscard: [() => api.targetDraftDiscard(3), 'target_draft_discard', { draftId: 3 }],
  });

  test('every exported function is covered, and together they call every command', async () => {
    const table = cases(new Channel<OpEvent>());
    const functions = Object.entries(api)
      // Every function but the error helpers calls a command.
      .filter(
        ([, value]) =>
          typeof value === 'function' &&
          value !== api.IpcError &&
          value !== api.uiErrorOf &&
          value !== api.onAuthRefusal,
      )
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

  test("op_execute sends the confirm dialog's password only when there is one", async () => {
    const channel = new Channel<OpEvent>();
    await api.opExecute(7, channel, 'open sesame');
    await api.opExecute(7, channel);
    expect(calls.map((c) => c.args)).toEqual([
      { opId: 7, onEvent: channel, password: 'open sesame' },
      { opId: 7, onEvent: channel },
    ]);
    expect(Object.keys(calls[1]?.args as object)).not.toContain('password');
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

describe('onAuthRefusal', () => {
  const refusal = (code: string | null): UiError => ({
    code,
    message: 'refused',
    help: null,
    causes: [],
    fields: {},
  });

  test('hears every authentication refusal, from whichever command, before the caller does', async () => {
    const heard: (string | null)[] = [];
    const off = api.onAuthRefusal((error) => heard.push(error.code));
    try {
      const codes = [
        DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
        DESKTOP_ERROR_CODES.AUTH_FAILED,
        DESKTOP_ERROR_CODES.AUTH_BUSY,
      ];
      const calls = [
        () => api.unlock(),
        () => api.unlockWithPassword('guess'),
        () => api.opExecute(7, new Channel<OpEvent>(), 'guess'),
      ];
      for (const [index, code] of codes.entries()) {
        answer = () => Promise.reject(refusal(code));
        const seen = await (calls[index] as () => Promise<unknown>)().catch(() => [...heard]);
        expect(seen).toEqual(codes.slice(0, index + 1));
      }
    } finally {
      off();
    }
  });

  test('not what says nothing about authentication: a cancel, the lock, a plain error', async () => {
    const heard: unknown[] = [];
    const off = api.onAuthRefusal((error) => heard.push(error));
    try {
      for (const code of [
        DESKTOP_ERROR_CODES.AUTH_CANCELLED,
        DESKTOP_ERROR_CODES.LOCKED,
        DESKTOP_ERROR_CODES.PLAN_NOT_FOUND,
        null,
      ]) {
        answer = () => Promise.reject(refusal(code));
        await api.unlock().catch(() => undefined);
      }
      expect(heard).toEqual([]);
    } finally {
      off();
    }
  });

  test('a listener that throws is reported and the caller still gets the refusal', async () => {
    const errors: unknown[] = [];
    const original = console.error;
    console.error = (...args: unknown[]) => errors.push(args);
    const off = api.onAuthRefusal(() => {
      throw new Error('listener bug');
    });
    try {
      answer = () => Promise.reject(refusal(DESKTOP_ERROR_CODES.AUTH_FAILED));
      const error = await api.unlock().catch((e: unknown) => e);
      expect((error as api.IpcError).error.code).toBe(DESKTOP_ERROR_CODES.AUTH_FAILED);
      expect(errors).toHaveLength(1);
    } finally {
      off();
      console.error = original;
    }
  });

  test('once its unsubscribe is called, a listener hears nothing more', async () => {
    const heard: unknown[] = [];
    api.onAuthRefusal((error) => heard.push(error))();
    answer = () => Promise.reject(refusal(DESKTOP_ERROR_CODES.AUTH_FAILED));
    await api.unlock().catch(() => undefined);
    expect(heard).toEqual([]);
  });
});

test('uiErrorOf reads an IpcError, a UiError, an Error or anything else as a UiError', () => {
  const locked: UiError = {
    code: DESKTOP_ERROR_CODES.LOCKED,
    message: 'AppRafter is locked.',
    help: null,
    causes: [],
    fields: {},
  };
  const bare = (message: string): UiError => ({
    code: null,
    message,
    help: null,
    causes: [],
    fields: {},
  });
  expect(api.uiErrorOf(new api.IpcError('unlock', locked))).toBe(locked);
  expect(api.uiErrorOf(locked)).toBe(locked);
  expect(api.uiErrorOf(new Error('boom'))).toEqual(bare('boom'));
  expect(api.uiErrorOf('refused')).toEqual(bare('refused'));
});
