// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Targets page on the mock IPC: target_list, and the reversible use plan run at once.
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { screen, waitFor, within } from '@testing-library/react';
import * as api from '../../ipc/api';
import { installMockIpc } from '../../ipc/mock';
import { resetOperations } from '../../ipc/operations';
import { startPlan } from '../../ipc/plans';
import { renderScreen } from '../../test/screens';
import { settleIpc } from '../../test/settle';
import { TargetsPage } from './TargetsPage';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  await settleIpc();
  resetOperations();
  clearMocks();
});

test('lists the store: cards, the unreadable one shown, the CLI default tagged', async () => {
  renderScreen(<TargetsPage onOpen={() => {}} openTargets={new Set()} />);
  expect(screen.getByRole('heading', { level: 1, name: 'Open a cluster' })).toBeDefined();
  expect(await screen.findByRole('article', { name: 'prod-eu' })).toBeDefined();
  expect(screen.getByRole('article', { name: 'broken' }).textContent).toContain('Cannot be read');
  expect(
    within(screen.getByRole('article', { name: 'prod-eu' })).getByText('CLI default'),
  ).toBeDefined();
  expect(
    within(screen.getByRole('article', { name: 'lab' })).queryByText('CLI default'),
  ).toBeNull();
  expect(screen.queryByText(/No data source yet/)).toBeNull();
  expect(screen.getByRole('button', { name: /Add target/ })).toBeDefined();
});

test('make default runs the reversible plan at once: no dialog, the tag moves, a toast', async () => {
  const user = renderScreen(<TargetsPage onOpen={() => {}} openTargets={new Set()} />);
  await user.click(await screen.findByRole('button', { name: 'Make staging the CLI default' }));
  expect(await screen.findByText('staging is the CLI default now')).toBeDefined();
  await waitFor(() =>
    expect(
      within(screen.getByRole('article', { name: 'staging' })).getByText('CLI default'),
    ).toBeDefined(),
  );
  expect(
    within(screen.getByRole('article', { name: 'prod-eu' })).queryByText('CLI default'),
  ).toBeNull();
  expect(screen.queryByRole('dialog')).toBeNull();
});

/** The args of every `cmd` the page sends from now on (the mock still answers it). */
function sentArgs(cmd: string): Record<string, unknown>[] {
  const internals = (
    window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown, options?: unknown) => unknown };
    }
  ).__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  const sent: Record<string, unknown>[] = [];
  internals.invoke = (c, args, options) => {
    if (c === cmd) sent.push({ ...(args as Record<string, unknown>) });
    return invoke(c, args, options);
  };
  return sent;
}

// Review #19: the card still offers Make default for a target that became the default since
// the page read the store (in a terminal): the plan changes nothing, so it is discarded unrun.
test('make default on a target that is the default already discards the plan unrun', async () => {
  const user = renderScreen(<TargetsPage onOpen={() => {}} openTargets={new Set()} />);
  const button = await screen.findByRole('button', { name: 'Make staging the CLI default' });
  const meanwhile = await api.opPlanTargetUse('staging');
  await (await startPlan(meanwhile.opId)).ended;
  const plans = sentArgs('op_plan_target_use');
  const runs = sentArgs('op_execute');
  const discards = sentArgs('op_discard');
  await user.click(button);
  expect(await screen.findByText('staging is already the CLI default')).toBeDefined();
  expect(plans).toHaveLength(1);
  expect(runs).toEqual([]);
  expect(discards).toHaveLength(1);
});

test('a refused make default is shown on the page', async () => {
  const user = renderScreen(<TargetsPage onOpen={() => {}} openTargets={new Set()} />);
  const button = await screen.findByRole('button', { name: 'Make lab the CLI default' });
  // Removed in a terminal meanwhile: the plan is refused with the names there are.
  clearMocks();
  mockIPC((cmd) =>
    cmd === 'op_plan_target_use'
      ? Promise.reject({
          code: 'apprafter::target::not_found',
          message: 'target `lab` not found (available: prod-eu, staging)',
          help: null,
          causes: [],
          fields: { name: 'lab', available: ['prod-eu', 'staging'] },
        })
      : null,
  );
  await user.click(button);
  expect((await screen.findByRole('alert')).textContent).toContain('target `lab` not found');
});

test('an open target switches to its tab; another opens one', async () => {
  const onOpen = mock();
  const user = renderScreen(<TargetsPage onOpen={onOpen} openTargets={new Set(['lab'])} />);
  await user.click(await screen.findByRole('button', { name: 'Switch to lab' }));
  await user.click(screen.getByRole('button', { name: 'Open staging' }));
  expect(onOpen.mock.calls).toEqual([['lab'], ['staging']]);
});

test('an empty store says how to add one, and a dangling CLI default is named', async () => {
  clearMocks();
  mockIPC((cmd) =>
    cmd === 'target_list'
      ? { targets: [], unreadable: [], cliDefault: { status: 'missing', name: 'gone' } }
      : null,
  );
  renderScreen(<TargetsPage onOpen={() => {}} openTargets={new Set()} />);
  expect(await screen.findByRole('heading', { name: 'No targets yet' })).toBeDefined();
  expect(screen.getByText('apprafter target add')).toBeDefined();
  expect(
    screen.getByText('The CLI default points at gone, which is not in the store.'),
  ).toBeDefined();
});

test('a CLI default that names an unreadable target: its card is tagged, and the page says so (review #12)', async () => {
  // In a terminal: broken (unreadable) became the CLI default.
  clearMocks();
  mockIPC((cmd) =>
    cmd === 'target_list'
      ? {
          targets: [],
          unreadable: [
            {
              name: 'broken',
              error: {
                code: 'apprafter::target::invalid_config',
                message: 'targets/broken/config.yaml: expected a mapping',
                help: null,
                causes: [],
                fields: {},
              },
            },
          ],
          cliDefault: { status: 'set', name: 'broken' },
        }
      : null,
  );
  renderScreen(<TargetsPage onOpen={() => {}} openTargets={new Set()} />);
  const card = await screen.findByRole('article', { name: 'broken' });
  expect(within(card).getByText('CLI default')).toBeDefined();
  expect(
    screen.getByText(
      'The CLI default points at broken, which cannot be read: CLI commands that name no target fail until it is fixed.',
    ),
  ).toBeDefined();
});

test('a store that cannot be read says why', async () => {
  clearMocks();
  mockIPC((cmd) =>
    cmd === 'target_list'
      ? Promise.reject({
          code: null,
          message: 'cannot read the target store',
          help: null,
          causes: [],
          fields: {},
        })
      : null,
  );
  renderScreen(<TargetsPage onOpen={() => {}} openTargets={new Set()} />);
  expect((await screen.findByRole('alert')).textContent).toContain('cannot read the target store');
  expect(screen.queryByRole('heading', { name: 'No targets yet' })).toBeNull();
});

test('the Add target card is enabled and opens the wizard', async () => {
  const user = renderScreen(<TargetsPage onOpen={() => {}} openTargets={new Set()} />);
  const add = (await screen.findByRole('button', { name: /Add target/ })) as HTMLButtonElement;
  expect(add.disabled).toBe(false);
  expect(add.textContent).toContain('Hetzner Cloud token, region, machine');
  expect(add.textContent).not.toContain('Arrives in D.3');
  await user.click(add);
  expect(await screen.findByRole('dialog', { name: 'Add target' })).toBeDefined();
});
