// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A confirmed plan runs on through a lock (Rust keeps it; app.rs), while the lock unmounts the
// screen that started it. Its end must still be shown: after the unlock, once it arrives.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { emit } from '@tauri-apps/api/event';
import { clearMocks, mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useEffect } from 'react';
import { PlanConfirm } from '../components/PlanConfirm';
import { ToastProvider, ToastViewport } from '../components/Toast';
import { endedAwaySnapshot, keepEndedAway, resetEndedAway } from '../ipc/away';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import { LOCK_CHANGED } from '../ipc/generated/events';
import { resetOperations } from '../ipc/operations';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo, lockState, planView } from '../test/fixtures';
import { settleIpc } from '../test/settle';
import { EndedAwayNotices } from './EndedAway';
import { LockGate } from './LockGate';
import { useOverlay, ViewFrame } from './ViewFrame';

const ALLOWED_LOCKED = [
  'lock_status',
  'app_info',
  'settings_get',
  'unlock',
  'quit',
  'window_ready',
];
let calls: { cmd: string; args: Record<string, unknown> }[];
let locked: boolean;
/** Rust's end of op 900, replayed to a follow: none yet, then the failure. */
let ended: boolean;
let opened: boolean;

beforeEach(() => {
  calls = [];
  locked = false;
  ended = false;
  opened = false;
  mockWindows('main');
  mockIPC(
    (cmd, raw) => {
      const args = (raw ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args });
      if (cmd === 'lock_status') return lockState({ locked });
      if (locked && !cmd.startsWith('plugin:') && !ALLOWED_LOCKED.includes(cmd)) {
        return Promise.reject({
          code: DESKTOP_ERROR_CODES.LOCKED,
          message: 'locked',
          help: null,
          causes: [],
          fields: {},
        });
      }
      if (cmd === 'op_execute') return 2; // Rust runs it; no event yet
      if (cmd === 'op_subscribe') {
        return {
          subscription: 3,
          replay: ended
            ? [
                {
                  kind: 'failed',
                  error: {
                    code: 'apprafter::provider::sku_unavailable',
                    message: 'cx22 is sold out in nbg1',
                    help: null,
                    causes: [],
                    fields: {},
                  },
                },
              ]
            : [],
        };
      }
      return null;
    },
    { shouldMockEvents: true },
  );
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  resetEndedAway();
  clearMocks();
});

/** Opens the confirm in the first shell only: the shell after the unlock is a new one. */
function OpenConfirm() {
  const show = useOverlay();
  useEffect(() => {
    if (opened) return;
    opened = true;
    show((close) => (
      <PlanConfirm
        view={planView({ opId: 900, title: 'Rename prod-eu' })}
        title="Rename prod-eu?"
        confirmLabel="Rename"
        auth={null}
        onDone={() => {}}
        onFailed={() => {}}
        onClose={close}
      />
    ));
  }, [show]);
  return null;
}

const changed = (isLocked: boolean) =>
  act(async () => {
    locked = isLocked;
    await emit(
      LOCK_CHANGED,
      lockState({ locked: isLocked, reason: isLocked ? 'manual' : null, seq: isLocked ? 1 : 2 }),
    );
    await new Promise((r) => setTimeout(r, 0));
  });

const discarded = () => calls.filter((c) => c.cmd === 'op_discard').map((c) => c.args.opId);

test('a confirmed plan that ends after a lock took its screen: its failure shows after the unlock', async () => {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <LockGate>
          <ToastProvider>
            <ViewFrame>
              <OpenConfirm />
            </ViewFrame>
            <EndedAwayNotices />
            <ToastViewport />
          </ToastProvider>
        </LockGate>
      </PlatformContext>
    </QueryClientProvider>,
  );
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Rename' }));
  expect(calls.map((c) => c.cmd)).toContain('op_execute');

  await changed(true);
  expect(screen.queryByRole('dialog', { name: 'Rename prod-eu?' })).toBeNull();
  ended = true; // it fails in Rust while the app is locked

  await changed(false);
  expect(await screen.findByText('Rename prod-eu failed: cx22 is sold out in nbg1')).toBeDefined();
  expect(discarded()).toEqual([900]);
  expect(endedAwaySnapshot()).toEqual([]);
});

test('ends kept away show one at a time, each discarded once shown', async () => {
  keepEndedAway({ opId: 31, text: 'Add target lab: done.', failed: false });
  keepEndedAway({ opId: 32, text: 'Rename prod failed: taken', failed: true });
  render(
    <ToastProvider>
      <EndedAwayNotices spacingMs={0} />
      <ToastViewport />
    </ToastProvider>,
  );
  expect(await screen.findByText('Add target lab: done.')).toBeDefined();
  expect(await screen.findByText('Rename prod failed: taken')).toBeDefined();
  expect(discarded()).toEqual([31, 32]);
  expect(endedAwaySnapshot()).toEqual([]);
});

test('each notice is given its time: the next waits for the spacing, and so does its discard', async () => {
  keepEndedAway({ opId: 41, text: 'Add target lab: done.', failed: false });
  keepEndedAway({ opId: 42, text: 'Rename prod failed: taken', failed: true });
  render(
    <ToastProvider schedule={() => () => {}}>
      <EndedAwayNotices spacingMs={300} />
      <ToastViewport />
    </ToastProvider>,
  );
  expect(await screen.findByText('Add target lab: done.')).toBeDefined();
  // Within the spacing: the first still shows, the second waits, unshown and not discarded.
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(screen.queryByText('Rename prod failed: taken') === null).toBe(true);
  expect(screen.queryByText('Add target lab: done.') === null).toBe(false);
  expect(discarded()).toEqual([41]);
  expect(await screen.findByText('Rename prod failed: taken', {}, { timeout: 2000 })).toBeDefined();
  expect(discarded()).toEqual([41, 42]);
});
