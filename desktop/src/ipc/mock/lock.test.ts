// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, describe, expect, test } from 'bun:test';
import { Channel } from '@tauri-apps/api/core';
import { clearMocks } from '@tauri-apps/api/mocks';
import { settleIpc } from '../../test/settle';
import * as api from '../api';
import { IpcError } from '../api';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import type { OpEvent } from '../generated/OpEvent';
import type { OpId } from '../generated/OpId';
import { installMockIpc } from './index';
import { MOCK_RECOVERY_KEY, MOCK_STORE_PASSWORD } from './lock';

// The settle rule (test/settle.ts; critic A finding 9): the engine's 0 ms events land before
// the mock goes.
afterEach(async () => {
  await settleIpc();
  clearMocks();
});

/** Runs plan `opId` (with the store's secret when given) and resolves with its last event. */
function ran(opId: OpId, password?: string): Promise<OpEvent> {
  return new Promise((resolve, reject) => {
    const channel = new Channel<OpEvent>();
    channel.onmessage = (event) => {
      if (event.kind === 'finished' || event.kind === 'failed') resolve(event);
    };
    api.opExecute(opId, channel, password).catch(reject);
  });
}

async function code(call: Promise<unknown>): Promise<string | null> {
  try {
    await call;
    return 'answered';
  } catch (e) {
    if (!(e instanceof IpcError)) throw e;
    return e.error.code;
  }
}

describe('the mock store lock', () => {
  test('a store in files: Protect needs its key typed back, then the secrets are protected', async () => {
    installMockIpc({ opDelayMs: 0 });
    await api.unlock();
    expect((await api.lockInfo()).report.mode).toBe('files');
    const plan = await api.opPlanLockOn('a lock password', null);
    expect(plan.recoveryKey).toBe(MOCK_RECOVERY_KEY);
    // Before "I saved it", not even the right key is taken; after it, no more copies (O12).
    expect(await code(api.opConfirmRecoveryKey(plan.plan.opId, MOCK_RECOVERY_KEY))).toBeNull();
    expect(await code(api.copyRecoveryKey(plan.plan.opId))).toBe('answered');
    await api.opRecoveryKeySaved(plan.plan.opId);
    expect(await code(api.copyRecoveryKey(plan.plan.opId))).toBeNull();
    expect(await code(api.opConfirmRecoveryKey(plan.plan.opId, 'not it'))).toBe(
      'apprafter::secrets::recovery_key_invalid',
    );
    await api.opConfirmRecoveryKey(plan.plan.opId, MOCK_RECOVERY_KEY);
    expect((await ran(plan.plan.opId)).kind).toBe('finished');
    const info = await api.appInfo();
    expect(info.secrets).toMatchObject({ mode: 'locked', protected: true });
  });

  test('Finish takes the recovery key as a recovery key, never the lock password twice', async () => {
    installMockIpc({ opDelayMs: 0, secrets: 'locking' });
    await api.unlockWithPassword(MOCK_STORE_PASSWORD);
    expect(await code(api.opPlanLockFinish(MOCK_STORE_PASSWORD, MOCK_STORE_PASSWORD))).toBe(
      DESKTOP_ERROR_CODES.AUTH_FAILED,
    );
    const finish = await api.opPlanLockFinish(MOCK_STORE_PASSWORD, MOCK_RECOVERY_KEY);
    expect((await ran(finish.opId)).kind).toBe('finished');
  });

  test('a wrong retype after the confirm leaves it unconfirmed, as in Rust', async () => {
    installMockIpc({ opDelayMs: 0 });
    await api.unlock();
    const plan = await api.opPlanLockOn('a lock password', null);
    await api.opRecoveryKeySaved(plan.plan.opId);
    await api.opConfirmRecoveryKey(plan.plan.opId, MOCK_RECOVERY_KEY);
    expect(await code(api.opConfirmRecoveryKey(plan.plan.opId, 'not it'))).toBe(
      'apprafter::secrets::recovery_key_invalid',
    );
    const end = await ran(plan.plan.opId);
    expect(end).toMatchObject({ kind: 'failed', error: { fields: { reason: 'not_confirmed' } } });
  });

  test('unconfirmed, the run fails as Rust refuses it', async () => {
    installMockIpc({ opDelayMs: 0 });
    await api.unlock();
    const plan = await api.opPlanLockOn('a lock password', null);
    const end = await ran(plan.plan.opId);
    expect(end).toMatchObject({ kind: 'failed', error: { fields: { reason: 'not_confirmed' } } });
  });

  test('a protected store: the field takes its secret, and Stop protecting asks for it', async () => {
    installMockIpc({ opDelayMs: 0, secrets: 'locked' });
    expect(await code(api.unlock())).toBe(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE);
    expect(await code(api.unlockWithPassword('wrong'))).toBe(DESKTOP_ERROR_CODES.AUTH_FAILED);
    await api.unlockWithPassword(MOCK_STORE_PASSWORD);
    const off = await api.opPlanLockOff();
    expect(off.plan.gesture).toBe('store_secret');
    expect(off.needsKey).toBe(false);
    expect(await code(ran(off.plan.opId))).toBe(DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE);
    expect((await ran(off.plan.opId, MOCK_RECOVERY_KEY)).kind).toBe('finished');
    expect((await api.appInfo()).secrets.mode).toBe('files');
  });
});
