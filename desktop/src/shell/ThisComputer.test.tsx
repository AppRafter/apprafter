// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Verification } from '../ipc/generated/Verification';
import { resetOperations } from '../ipc/operations';
import { PlatformContext } from '../state/platform';
import { createQueryClient } from '../state/queryClient';
import { appInfo } from '../test/fixtures';
import { whoamiReport } from '../test/flows';
import { cancelled, completed, type Harness, installHarness, uiError } from '../test/ipc';
import { settleIpc } from '../test/settle';
import { cliDefaultLine, IDENTITY, ThisComputerRow, verificationLine } from './ThisComputer';

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

function renderRow() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <ThisComputerRow />
      </PlatformContext>
    </QueryClientProvider>,
  );
  return userEvent.setup();
}

describe('This computer', () => {
  test('a failed or skipped verification is never shown as verified (guard)', () => {
    const notVerified: Verification[] = [
      { status: 'skipped', reason: 'no_ping' },
      { status: 'skipped', reason: 'no_token' },
      { status: 'skipped', reason: 'unsupported_provider' },
      { status: 'rejected' },
      { status: 'http_error', httpStatus: 503 },
      { status: 'unreachable' },
    ];
    for (const v of notVerified) {
      const line = verificationLine(v);
      expect({ v, verified: /verified/i.test(line.text) && !/not/i.test(line.text) }).toEqual({
        v,
        verified: false,
      });
      expect({ v, ok: line.tone === 'ok' }).toEqual({ v, ok: false });
    }
    expect(verificationLine({ status: 'verified', elapsedMs: 182 })).toEqual({
      tone: 'ok',
      text: 'Token verified · 182 ms',
    });
    expect(verificationLine({ status: 'http_error', httpStatus: 503 }).text).toBe(
      'The provider answered HTTP 503',
    );
  });

  test('lines: identity, the CLI default found, missing or none', () => {
    expect(IDENTITY.anonymous_self_hosted).toBe('anonymous (self-hosted)');
    expect(cliDefaultLine({ status: 'none' })).toBe(
      'No CLI default: add a target, or make one the default on the Targets page.',
    );
    expect(cliDefaultLine({ status: 'missing', name: 'gone', available: ['a', 'b'] })).toBe(
      'The CLI default `gone` does not exist (targets: a, b).',
    );
    expect(cliDefaultLine({ status: 'missing', name: 'gone', available: [] })).toBe(
      'The CLI default `gone` does not exist.',
    );
    expect(cliDefaultLine(whoamiReport({ status: 'skipped', reason: 'no_ping' }).cliDefault)).toBe(
      'CLI default: prod-eu · hetzner-cloud · nbg1',
    );
  });

  test('the row reads whoami without a ping; Verify pings through an operation', async () => {
    h.answer('whoami', whoamiReport({ status: 'skipped', reason: 'no_ping' }));
    h.read('op_start_whoami', [completed(whoamiReport({ status: 'verified', elapsedMs: 182 }))]);
    const user = renderRow();
    expect(await screen.findByText('anonymous (self-hosted)')).toBeDefined();
    expect(screen.getByText('Not checked yet')).toBeDefined();
    expect(screen.getByText('cx22 · ~/.ssh/id_ed25519.pub · ssh-ed25519')).toBeDefined();
    expect(h.of('op_start_whoami')).toHaveLength(0);
    await user.click(screen.getByRole('button', { name: 'Verify' }));
    expect(await screen.findByText('Token verified · 182 ms')).toBeDefined();
    expect(h.of('op_start_whoami')).toHaveLength(1);
  });

  test('a rejected token reads as rejected, in the error tone', async () => {
    h.answer('whoami', whoamiReport({ status: 'skipped', reason: 'no_ping' }));
    h.read('op_start_whoami', [completed(whoamiReport({ status: 'rejected' }))]);
    const user = renderRow();
    await user.click(await screen.findByRole('button', { name: 'Verify' }));
    const line = await screen.findByText('Token rejected (HTTP 401)');
    expect(line.getAttribute('data-tone')).toBe('err');
  });

  test('a key that is gone, and no machine yet, are said as such', async () => {
    const report = whoamiReport({ status: 'skipped', reason: 'no_ping' });
    if (report.cliDefault.status !== 'found') throw new Error('fixture');
    h.answer('whoami', {
      ...report,
      cliDefault: {
        status: 'found',
        target: {
          ...report.cliDefault.target,
          serverType: null,
          sshKey: {
            path: '/gone.pub',
            display: '/gone.pub',
            exists: false,
            algo: null,
            problem: 'missing',
          },
        },
      },
    });
    renderRow();
    expect(await screen.findByText('no server type · /gone.pub (missing)')).toBeDefined();
  });

  test('a private key set as the key is said as such, never shown as the key in use', async () => {
    const report = whoamiReport({ status: 'skipped', reason: 'no_ping' });
    if (report.cliDefault.status !== 'found') throw new Error('fixture');
    h.answer('whoami', {
      ...report,
      cliDefault: {
        status: 'found',
        target: {
          ...report.cliDefault.target,
          sshKey: {
            path: '/home/alex/.ssh/id_ed25519',
            display: '~/.ssh/id_ed25519',
            exists: true,
            algo: null,
            problem: 'private_key',
          },
        },
      },
    });
    renderRow();
    expect(
      await screen.findByText('cx22 · ~/.ssh/id_ed25519 (a private key: not used)'),
    ).toBeDefined();
  });

  test('a ping that fails, or is cancelled, says so', async () => {
    h.answer('whoami', whoamiReport({ status: 'skipped', reason: 'no_ping' }));
    h.answer('op_start_whoami', () =>
      Promise.reject(uiError('apprafter::desktop::internal', 'the ping broke')),
    );
    const user = renderRow();
    await user.click(await screen.findByRole('button', { name: 'Verify' }));
    expect(await screen.findByText('the ping broke')).toBeDefined();
    h.answer('op_start_whoami', 77);
    h.operation(77, [cancelled()]);
    await user.click(screen.getByRole('button', { name: 'Verify' }));
    expect(await screen.findByText('The check was cancelled.')).toBeDefined();
  });

  test('a whoami that cannot be read says why, rather than reading forever', async () => {
    h.answer('whoami', () =>
      Promise.reject(uiError('apprafter::desktop::internal', 'the store cannot be read')),
    );
    renderRow();
    expect(await screen.findByText('the store cannot be read')).toBeDefined();
    expect(screen.queryByText('Reading…')).toBeNull();
  });

  test('no CLI default: no Verify button', async () => {
    h.answer('whoami', { identity: 'anonymous_self_hosted', cliDefault: { status: 'none' } });
    renderRow();
    await screen.findByText(/No CLI default/);
    expect(screen.queryByRole('button', { name: 'Verify' })).toBeNull();
  });
});
