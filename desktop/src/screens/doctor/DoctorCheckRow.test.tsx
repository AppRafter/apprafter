// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { Check } from '../../ipc/generated/Check';
import type { CheckFix } from '../../ipc/generated/CheckFix';
import { check } from '../../test/flows';
import { DoctorCheckRow } from './DoctorCheckRow';

function row(c: Check, onAction?: (fix: CheckFix) => void) {
  const { container } = render(
    <ul>
      {onAction === undefined ? (
        <DoctorCheckRow check={c} target="prod-eu" />
      ) : (
        <DoctorCheckRow check={c} target="prod-eu" onAction={onAction} />
      )}
    </ul>,
  );
  const item = container.querySelector('li.doctor-row');
  if (item === null) throw new Error('no row');
  return { item, user: userEvent.setup() };
}

const helm = check({
  id: 'tool',
  tool: 'helm',
  status: 'warn',
  title: '`helm` on PATH',
  fix: { kind: 'install_tool', tool: 'helm' },
});

describe('DoctorCheckRow', () => {
  test('a failed row: a tinted FAIL badge, the title, the detail and the fix', () => {
    const { item } = row(
      check({
        id: 'token_verified',
        status: 'fail',
        title: 'Token verified against provider API',
        detail: 'HTTP 401: Unable to authenticate',
        fix: { kind: 'renew_token', target: 'prod-eu', why: 'token_rejected' },
      }),
    );
    const badge = item.querySelector('.tag');
    expect(badge?.textContent).toBe('FAIL');
    expect(badge?.getAttribute('data-tone')).toBe('err');
    expect(badge?.getAttribute('data-variant')).toBe('tinted');
    expect(item.querySelector('.doctor-title')?.textContent).toBe(
      'Token verified against provider API',
    );
    expect(item.querySelector('.doctor-detail')?.textContent).toBe(
      'HTTP 401: Unable to authenticate',
    );
    expect(item.querySelector('.doctor-fix')?.textContent).toStartWith(
      'The provider rejected the stored token.',
    );
  });

  test('backticks in the title, the detail and the fix are code', () => {
    const { item } = row(
      check({
        id: 'kube_api_reachable',
        status: 'skipped',
        title: 'Kube API reachable',
        detail: '`kubectl` not found',
        fix: { kind: 'install_tool', tool: 'kubectl' },
      }),
    );
    expect(item.querySelector('.doctor-detail code')?.textContent).toBe('kubectl');
    expect(item.querySelector('.doctor-fix code')?.textContent).toBe('kubectl');
    const { item: tool } = row(helm);
    expect(tool.querySelector('.doctor-title code')?.textContent).toBe('helm');
    expect(tool.querySelector('.doctor-title')?.textContent).toBe('helm on PATH');
  });

  test('a skipped row: a neutral SKIP badge and the reason it did not run', () => {
    const { item } = row(
      check({
        id: 'kubeconfig_cached',
        status: 'skipped',
        title: 'Kubeconfig cached',
        detail: 'no provisioned server',
      }),
    );
    expect(item.querySelector('.tag')?.textContent).toBe('SKIP');
    expect(item.querySelector('.tag')?.getAttribute('data-tone')).toBe('neutral');
    expect(item.querySelector('.doctor-detail')?.textContent).toBe('no provisioned server');
  });

  test('a passing row with no detail and no fix shows neither', () => {
    const { item } = row(check());
    expect(item.querySelector('.tag')?.getAttribute('data-tone')).toBe('ok');
    expect(item.querySelector('.doctor-detail')).toBeNull();
    expect(item.querySelector('.doctor-fix')).toBeNull();
  });

  test('a missing tool offers the toolchain, a missing target the wizard; each passes its fix', async () => {
    const onAction = mock();
    const { user } = row(helm, onAction);
    await user.click(screen.getByRole('button', { name: 'Show the toolchain' }));
    expect(onAction).toHaveBeenLastCalledWith({ kind: 'install_tool', tool: 'helm' });
    const target: CheckFix = { kind: 'add_target', name: null, available: [] };
    row(
      check({ id: 'active_target', status: 'fail', title: 'Active target set', fix: target }),
      onAction,
    );
    await user.click(screen.getByRole('button', { name: 'Add a target' }));
    expect(onAction).toHaveBeenLastCalledWith(target);
  });

  test('a target with no SSH key offers to change it, passing its fix', async () => {
    const onAction = mock();
    const fix: CheckFix = { kind: 'configure_ssh_key', target: 'prod-eu' };
    const { user } = row(
      check({ id: 'ssh_key', status: 'warn', title: 'SSH key path configured', fix }),
      onAction,
    );
    await user.click(screen.getByRole('button', { name: 'Change SSH key' }));
    expect(onAction).toHaveBeenLastCalledWith(fix);
  });

  test('a fix with no screen has no button; without onAction no fix has one', () => {
    row(
      check({
        status: 'fail',
        fix: { kind: 'renew_token', target: 'prod-eu', why: 'token_rejected' },
      }),
      mock(),
    );
    expect(screen.queryByRole('button')).toBeNull();
    row(helm);
    expect(screen.queryByRole('button')).toBeNull();
  });
});
