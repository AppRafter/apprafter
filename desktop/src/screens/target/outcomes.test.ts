// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { removedMessage, renamedMessage, renewedMessage, usedMessage } from './outcomes';

test('each outcome says what moved: the CLI default and a running server included', () => {
  expect(renamedMessage({ from: 'prod', to: 'prod-eu', stateMoved: true, cliDefault: null })).toBe(
    'Renamed prod to prod-eu',
  );
  expect(
    renamedMessage({
      from: 'prod',
      to: 'prod-eu',
      stateMoved: true,
      cliDefault: { from: 'prod', to: 'prod-eu' },
    }),
  ).toBe('Renamed prod to prod-eu · the CLI default is now prod-eu');
  expect(
    removedMessage({
      name: 'prod',
      stateRemoved: true,
      orphanedServer: null,
      cliDefault: { from: 'prod', to: null },
    }),
  ).toBe('Removed prod from this computer · no CLI default now');
  expect(
    removedMessage({
      name: 'prod',
      stateRemoved: true,
      cliDefault: { from: 'prod', to: 'lab' },
      orphanedServer: { serverId: 4711, serverName: 'prod-1', serverType: 'cx22' },
    }),
  ).toBe(
    'Removed prod from this computer · server prod-1 keeps running at the provider · the CLI default is now lab',
  );
  expect(
    removedMessage({ name: 'prod', stateRemoved: false, orphanedServer: null, cliDefault: null }),
  ).toBe('Removed prod from this computer');
  expect(usedMessage({ name: 'lab', pointer: { from: 'prod', to: 'lab' } })).toBe(
    'lab is the CLI default now',
  );
  expect(usedMessage({ name: 'lab', pointer: null })).toBe('lab is already the CLI default');
  expect(
    renewedMessage({
      name: 'prod',
      sshKeyChanged: false,
      token: { status: 'verified', elapsedMs: 182 },
    }),
  ).toBe('Token renewed · verified with the provider');
  expect(
    renewedMessage({
      name: 'prod',
      sshKeyChanged: false,
      token: { status: 'skipped', reason: 'no_ping' },
    }),
  ).toBe('Token renewed');
  expect(
    renewedMessage({
      name: 'prod',
      sshKeyChanged: true,
      token: { status: 'verified', elapsedMs: 182 },
    }),
  ).toBe('SSH key changed, token renewed · verified with the provider');
});
