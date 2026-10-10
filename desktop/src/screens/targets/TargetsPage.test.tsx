// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Targets page on the mock IPC: target_list, and the reversible use plan run at once.
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { screen, waitFor, within } from '@testing-library/react';
import * as api from '../../ipc/api';
import { installMockIpc } from '../../ipc/mock';
import { resetOperations } from '../../ipc/operations';
import { renderScreen } from '../../test/screens';
import { TargetsPage } from './TargetsPage';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(() => {
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
