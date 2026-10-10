// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Target screen's rows (TargetDetails, MachineRow), its Danger zone, and the Targets page's
// card for a target that cannot be read.
import { expect, mock, test } from 'bun:test';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { provisioned, targetReport } from '../../test/fixtures';
import { UnreadableCard } from '../targets/UnreadableCard';
import { DangerZone } from './DangerZone';
import { TargetDetails } from './TargetDetails';

const row = (label: string) => screen.getByRole('group', { name: label });
const noop = () => ({ rename: mock(), renew: mock(), makeDefault: mock(), changeSshKey: mock() });

test('the rows the CLI prints, with its "not set", and no token anywhere', () => {
  render(
    <TargetDetails
      report={targetReport()}
      os="linux"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={null}
    />,
  );
  expect(screen.getByRole('region', { name: 'Target' })).toBeDefined();
  expect(screen.getAllByRole('group').map((g) => g.getAttribute('aria-label'))).toEqual([
    'Name',
    'CLI default',
    'Provider',
    'Region',
    'Default tier',
    'Cluster name',
    'Machine',
    'API token',
    'SSH key',
    'Config file',
    'Credentials file',
  ]);
  expect(row('Name').textContent).toContain('prod-eu');
  expect(row('CLI default').textContent).toContain('Yes');
  expect(row('Provider').textContent).toContain('Hetzner Cloud');
  expect(row('Region').textContent).toContain('nbg1');
  expect(row('Default tier').textContent).toContain('Team (T2)');
  expect(row('Cluster name').textContent).toContain('not set');
  expect(row('Machine').textContent).toBe('Machinecx22');
  expect(row('API token').textContent).toContain('set · 64 characters');
  expect(row('API token').textContent).toContain('Saved in a file readable only by you');
  expect(row('SSH key').textContent).toContain('~/.ssh/id_ed25519.pub · ssh-ed25519');
  expect(row('Config file').textContent).toContain(
    '~/.config/apprafter/targets/prod-eu/config.yaml',
  );
  expect(document.body.textContent).not.toContain('encrypted');
});

test('the values that can be cut short in a narrow card carry their whole text as a title', () => {
  render(
    <TargetDetails
      report={targetReport()}
      os="linux"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={null}
    />,
  );
  const titleOf = (label: string) =>
    row(label).querySelector('.row-value [title]')?.getAttribute('title');
  expect(titleOf('SSH key')).toBe('~/.ssh/id_ed25519.pub · ssh-ed25519');
  expect(titleOf('Config file')).toBe('~/.config/apprafter/targets/prod-eu/config.yaml');
  expect(titleOf('Credentials file')).toBe('~/.config/apprafter/targets/prod-eu/credentials.yaml');
});

test('a stored key file the core reads no public key in says so, with no type (GOTCHA-149)', () => {
  for (const [problem, said] of [
    ['private_key', '~/.ssh/id_ed25519 (a private key: not used)'],
    ['not_public_key', '~/.ssh/id_ed25519 (not a public key)'],
    ['unreadable', '~/.ssh/id_ed25519 (cannot be read)'],
  ] as const) {
    const { unmount } = render(
      <TargetDetails
        report={targetReport({
          sshKey: {
            path: '/home/alex/.ssh/id_ed25519',
            display: '~/.ssh/id_ed25519',
            exists: true,
            algo: null,
            problem,
          },
        })}
        os="linux"
        secretBackend="file"
        actions={noop()}
        onChangeMachine={null}
      />,
    );
    expect(row('SSH key').textContent).toContain(said);
    unmount();
  }
});

test('what is not stored reads "not set"; a key file that went is "missing"', () => {
  const report = targetReport({
    region: null,
    serverType: null,
    defaultTier: null,
    tierLevel: null,
    token: { set: false, chars: null },
    sshKey: {
      path: '/home/alex/.ssh/gone.pub',
      display: '~/.ssh/gone.pub',
      exists: false,
      algo: null,
      problem: 'missing',
    },
  });
  render(
    <TargetDetails
      report={report}
      os="linux"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={null}
    />,
  );
  expect(row('Region').textContent).toContain('not set');
  expect(row('Default tier').textContent).toContain('not set');
  expect(row('Machine').textContent).toContain('not set');
  expect(row('API token').textContent).toBe('API tokennot setRenew');
  expect(row('SSH key').textContent).toContain('~/.ssh/gone.pub (missing)');
});

test('Windows: the file backend does not claim "readable only by you"', () => {
  render(
    <TargetDetails
      report={targetReport()}
      os="windows"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={null}
    />,
  );
  expect(row('API token').textContent).toContain('Saved in a file in your user profile');
  expect(row('API token').textContent).not.toContain('readable only by you');
});

test('a provisioned target: no Change, the refusal and the rebuild path, the running type', () => {
  const report = targetReport({ serverType: 'cx22', provisioned: provisioned() });
  render(
    <TargetDetails
      report={report}
      os="linux"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={() => {}}
    />,
  );
  const machine = row('Machine');
  expect(within(machine).queryByRole('button', { name: 'Change' })).toBeNull();
  expect(machine.textContent).toContain('cx22 · running cpx22');
  expect(machine.textContent).toContain('prod-eu-1 (id 4711)');
  // The CLI's recipe (render::core_error::resize_recipe): restore needs its repo, after destroy.
  expect(machine.textContent).toContain('apprafter target use prod-eu');
  expect(machine.textContent).toContain('apprafter backup create --repo <repo>');
  expect(machine.textContent).toContain('apprafter destroy --yes');
  expect(machine.textContent).toContain('apprafter restore <repo> --reprovision --server-type');
  expect(machine.textContent).toContain('not only this cluster');
});

test('a server recorded with the configured type shows the type once', () => {
  const report = targetReport({ provisioned: provisioned({ serverType: 'cx22' }) });
  render(
    <TargetDetails
      report={report}
      os="linux"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={null}
    />,
  );
  expect(row('Machine').querySelector('.row-value')?.textContent).toBe('cx22');
  expect(row('Machine').textContent).toContain('Provisioned as prod-eu-1');
});

test('not provisioned: Change only when a picker is given; unreadable state: never', async () => {
  const onChangeMachine = mock();
  const { rerender } = render(
    <TargetDetails
      report={targetReport()}
      os="linux"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={onChangeMachine}
    />,
  );
  await userEvent.setup().click(within(row('Machine')).getByRole('button', { name: 'Change' }));
  expect(onChangeMachine).toHaveBeenCalled();
  rerender(
    <TargetDetails
      report={targetReport()}
      os="linux"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={null}
    />,
  );
  expect(within(row('Machine')).queryByRole('button', { name: 'Change' })).toBeNull();
  const unreadable = targetReport({
    provisioned: {
      status: 'unreadable',
      error: {
        code: 'apprafter::state::corrupt',
        message: 'state.json: expected value',
        help: null,
        causes: [],
        fields: {},
      },
    },
  });
  rerender(
    <TargetDetails
      report={unreadable}
      os="linux"
      secretBackend="file"
      actions={noop()}
      onChangeMachine={() => {}}
    />,
  );
  expect(within(row('Machine')).queryByRole('button', { name: 'Change' })).toBeNull();
  expect(row('Machine').textContent).toContain('state.json: expected value');
});

test('the SSH key row offers Change, set or not', async () => {
  const actions = noop();
  const { rerender } = render(
    <TargetDetails
      report={targetReport()}
      os="linux"
      secretBackend="file"
      actions={actions}
      onChangeMachine={null}
    />,
  );
  const user = userEvent.setup();
  await user.click(within(row('SSH key')).getByRole('button', { name: 'Change' }));
  expect(actions.changeSshKey).toHaveBeenCalledTimes(1);
  rerender(
    <TargetDetails
      report={targetReport({ sshKey: null })}
      os="linux"
      secretBackend="file"
      actions={actions}
      onChangeMachine={null}
    />,
  );
  expect(row('SSH key').textContent).toContain('not set');
  await user.click(within(row('SSH key')).getByRole('button', { name: 'Change' }));
  expect(actions.changeSshKey).toHaveBeenCalledTimes(2);
});

test('Make default only on another target; Rename and Renew call back', async () => {
  const actions = noop();
  const { rerender } = render(
    <TargetDetails
      report={targetReport({ isCliDefault: false })}
      os="linux"
      secretBackend="file"
      actions={actions}
      onChangeMachine={null}
    />,
  );
  expect(row('CLI default').textContent).toContain('No');
  const user = userEvent.setup();
  await user.click(within(row('CLI default')).getByRole('button', { name: 'Make default' }));
  await user.click(within(row('Name')).getByRole('button', { name: 'Rename' }));
  await user.click(within(row('API token')).getByRole('button', { name: 'Renew' }));
  expect(
    [actions.makeDefault, actions.rename, actions.renew].map((f) => f.mock.calls.length),
  ).toEqual([1, 1, 1]);
  rerender(
    <TargetDetails
      report={targetReport()}
      os="linux"
      secretBackend="file"
      actions={actions}
      onChangeMachine={null}
    />,
  );
  expect(within(row('CLI default')).queryByRole('button')).toBeNull();
});

test('the danger zone removes from this computer and says the provider is untouched', async () => {
  const onRemove = mock();
  render(<DangerZone onRemove={onRemove} />);
  const zone = screen.getByRole('region', { name: 'Danger zone' });
  expect(zone.getAttribute('data-tone')).toBe('danger');
  expect(zone.textContent).toContain('Nothing changes at the provider');
  await userEvent.setup().click(within(zone).getByRole('button', { name: 'Remove…' }));
  expect(onRemove).toHaveBeenCalled();
});

test('an unreadable target is shown, with what the store said, and offers nothing', () => {
  render(
    <UnreadableCard
      target={{
        name: 'broken',
        error: {
          code: 'apprafter::target::invalid_config',
          message: 'targets/broken/config.yaml: invalid YAML',
          help: null,
          causes: [],
          fields: {},
        },
      }}
    />,
  );
  const card = screen.getByRole('article', { name: 'broken' });
  expect(card.getAttribute('data-state')).toBe('unreadable');
  expect(card.textContent).toContain('Cannot be read');
  expect(card.textContent).toContain('invalid YAML');
  expect(card.textContent).toContain('apprafter::target::invalid_config');
  expect(within(card).queryByRole('button')).toBeNull();
});
