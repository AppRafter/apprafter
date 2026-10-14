// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, screen, waitFor } from '@testing-library/react';
import type { PlanView } from '../../ipc/generated/PlanView';
import { resetOperations } from '../../ipc/operations';
import { appInfo } from '../../test/fixtures';
import { completed, type Harness, installHarness } from '../../test/ipc';
import { renderScreen } from '../../test/screens';
import { settleIpc } from '../../test/settle';
import { StopProtectingDialog } from './StopProtectingDialog';

let h: Harness;
beforeEach(() => {
  h = installHarness();
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  clearMocks();
});

const viewOf = (opId: number, gesture: PlanView['gesture'] = 'store_secret'): PlanView => ({
  opId,
  class: 'destructive',
  title: 'Put the secrets of the target store at ~/s back in files',
  changes: [{ kind: 'Store', object: '~/s/vault', action: 'delete', detail: null }],
  target: null,
  expiresAtMs: Date.now() + 600_000,
  gesture,
});

test('it warns that the secrets will be in clear, names the destination, and runs with the secret', async () => {
  const opId = h.newOperation([completed({ notLocked: false })]);
  const onDone = mock();
  const user = renderScreen(
    <StopProtectingDialog
      view={viewOf(opId)}
      needsKey
      finishing={false}
      auth={appInfo().auth}
      onDone={onDone}
      onFailed={mock()}
      onClose={mock()}
    />,
  );
  expect(screen.getByRole('heading', { name: 'Stop protecting secrets' })).toBeDefined();
  expect(screen.getByText(/on disk in clear again/)).toBeDefined();
  expect(screen.getByText(/^The Hetzner tokens go back to credentials.yaml/)).toBeDefined();
  expect(screen.getByText(/AppRafter does not read APPRAFTER_AGE_KEY/)).toBeDefined();
  await user.type(screen.getByLabelText('Lock password or recovery key'), 'the lock password');
  await user.click(screen.getByRole('button', { name: 'Stop protecting' }));
  await waitFor(() => expect(onDone).toHaveBeenCalled());
  expect(h.of('op_execute')[0]?.args.password).toBe('the lock password');
});

test('with no cache to re-seal it names no key, and no terminal for another (d4a-f Task 3)', () => {
  renderScreen(
    <StopProtectingDialog
      view={viewOf(h.newOperation([]))}
      needsKey={false}
      finishing={false}
      auth={appInfo().auth}
      onDone={mock()}
      onFailed={mock()}
      onClose={mock()}
    />,
  );
  expect(screen.getByText(/on disk in clear again/)).toBeDefined();
  expect(screen.getByText(/^The Hetzner tokens go back to credentials.yaml/)).toBeDefined();
  expect(screen.queryByText(/age key|APPRAFTER_AGE_KEY|apprafter lock off/)).toBeNull();
});

test('finishing an interrupted stop says so', () => {
  renderScreen(
    <StopProtectingDialog
      view={viewOf(h.newOperation([]))}
      needsKey={false}
      finishing
      auth={appInfo().auth}
      onDone={mock()}
      onFailed={mock()}
      onClose={mock()}
    />,
  );
  expect(
    screen.getByRole('heading', { name: 'Finish putting secrets back in files' }),
  ).toBeDefined();
});

test('with nothing to open says no secret moves, and warns of no clear text (UI verifier minor 2)', () => {
  renderScreen(
    <StopProtectingDialog
      view={viewOf(h.newOperation([]), 'none')}
      needsKey={false}
      finishing
      auth={appInfo().auth}
      onDone={mock()}
      onFailed={mock()}
      onClose={mock()}
    />,
  );
  expect(screen.getByText(/^No vault is there to open, so no secret moves/)).toBeDefined();
  expect(screen.queryByText(/on disk in clear again/)).toBeNull();
});
