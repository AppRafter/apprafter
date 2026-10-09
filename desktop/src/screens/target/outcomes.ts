// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The toast after a target operation: what it did, and what moved with it — the CLI default,
// and a server that keeps running at the provider.
import type { ActivePointerChange } from '../../ipc/generated/ActivePointerChange';
import type { TargetRemoved } from '../../ipc/generated/TargetRemoved';
import type { TargetRenamed } from '../../ipc/generated/TargetRenamed';
import type { TargetRenewed } from '../../ipc/generated/TargetRenewed';
import type { TargetUsed } from '../../ipc/generated/TargetUsed';

/** Where the CLI default went, when an operation moved it. */
const pointerPart = (change: ActivePointerChange | null): string[] => {
  if (change === null) return [];
  return [change.to === null ? 'no CLI default now' : `the CLI default is now ${change.to}`];
};

export function renamedMessage(outcome: TargetRenamed): string {
  return [`Renamed ${outcome.from} to ${outcome.to}`, ...pointerPart(outcome.cliDefault)].join(
    ' · ',
  );
}

export function removedMessage(outcome: TargetRemoved): string {
  const server = outcome.orphanedServer;
  return [
    `Removed ${outcome.name} from this computer`,
    ...(server === null ? [] : [`server ${server.serverName} keeps running at the provider`]),
    ...pointerPart(outcome.cliDefault),
  ].join(' · ');
}

export function usedMessage(outcome: TargetUsed): string {
  return outcome.pointer === null
    ? `${outcome.name} is already the CLI default`
    : `${outcome.name} is the CLI default now`;
}

export function renewedMessage(outcome: TargetRenewed): string {
  return outcome.token.status === 'verified'
    ? 'Token renewed · verified with the provider'
    : 'Token renewed';
}
