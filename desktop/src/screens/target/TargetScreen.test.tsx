// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The Target screen on the mock IPC: the report, and each action by its plan class — make
// default at once, rename / renew / the SSH key a plain confirm listing the changes (the SSH key
// alone, no token), remove the full plan with the typed name and the gesture.
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Activity } from 'react';
import { ToastProvider } from '../../components/Toast';
import * as api from '../../ipc/api';
import { CORE_ERROR_CODES } from '../../ipc/errors';
import { installMockIpc } from '../../ipc/mock';
import { resetOperations } from '../../ipc/operations';
import { startPlan } from '../../ipc/plans';
import { AppOverlayContext, useAppOverlayHost } from '../../shell/AppOverlays';
import { ViewFrame } from '../../shell/ViewFrame';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo } from '../../test/fixtures';
import { uiError } from '../../test/ipc';
import { renderScreen } from '../../test/screens';
import { settleIpc } from '../../test/settle';
import { TargetScreen } from './TargetScreen';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  await settleIpc();
  resetOperations();
  clearMocks();
});

const screenOf = (name: string, more: { onRenamed?: () => void; onRemoved?: () => void } = {}) =>
  renderScreen(
    <TargetScreen
      name={name}
      onRenamed={more.onRenamed ?? (() => {})}
      onRemoved={more.onRemoved ?? (() => {})}
    />,
  );

test('shows the report; a provisioned target offers no machine change', async () => {
  screenOf('prod-eu');
  expect(await screen.findByRole('group', { name: 'Machine' })).toBeDefined();
  expect(screen.getByRole('heading', { level: 1, name: 'Target' })).toBeDefined();
  expect(screen.getByText(/How this computer reaches prod-eu/)).toBeDefined();
  expect(screen.getByRole('group', { name: 'Machine' }).textContent).toContain(
    'apprafter backup create',
  );
  expect(screen.getByRole('region', { name: 'Danger zone' })).toBeDefined();
});

test('rename: the form checks the name, a plain confirm lists the changes, the tab follows', async () => {
  const onRenamed = mock();
  const user = screenOf('prod-eu', { onRenamed });
  await user.click(await screen.findByRole('button', { name: 'Rename' }));
  const form = screen.getByRole('dialog', { name: 'Rename target' });
  const field = within(form).getByLabelText('New name');
  expect(within(form).getByText('That is its name now.')).toBeDefined();
  await user.clear(field);
  await user.type(field, 'bad name');
  expect(within(form).getByText('Letters, digits and dashes only.')).toBeDefined();
  await user.clear(field);
  await user.type(field, 'prod-de');
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  const confirm = await screen.findByRole('dialog', { name: 'Rename prod-eu to prod-de?' });
  expect(confirm.textContent).toContain('CLI default');
  expect(screen.queryByRole('dialog', { name: 'Rename target' })).toBeNull();
  await user.click(within(confirm).getByRole('button', { name: 'Rename' }));
  await waitFor(() => expect(onRenamed).toHaveBeenCalledWith('prod-eu', 'prod-de'));
  expect(
    await screen.findByText('Renamed prod-eu to prod-de · the CLI default is now prod-de'),
  ).toBeDefined();
});

test('a taken name is refused in the form, which stays open', async () => {
  const user = screenOf('prod-eu');
  await user.click(await screen.findByRole('button', { name: 'Rename' }));
  const form = screen.getByRole('dialog', { name: 'Rename target' });
  await user.clear(within(form).getByLabelText('New name'));
  await user.type(within(form).getByLabelText('New name'), 'staging');
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  expect((await within(form).findByRole('alert')).textContent).toContain(
    'target `staging` already exists',
  );
  expect(screen.queryByRole('dialog', { name: /Rename prod-eu to/ })).toBeNull();
});

test('renew: the token is checked in the form, then the renewal is confirmed and runs', async () => {
  const user = screenOf('staging');
  await user.click(await screen.findByRole('button', { name: 'Renew' }));
  const form = screen.getByRole('dialog', { name: 'Renew API token' });
  const field = within(form).getByLabelText('Hetzner Cloud token');
  await user.type(field, 'short');
  expect(within(form).getByText(/this one has 5\./)).toBeDefined();
  await user.clear(field);
  await user.type(field, 'k'.repeat(64)); // token-shaped, nobody's
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  const confirm = await screen.findByRole('dialog', { name: 'Renew the API token of staging?' });
  expect(confirm.textContent).toContain('API token');
  expect(screen.queryByDisplayValue('k'.repeat(64))).toBeNull(); // the form, and its copy, are gone
  await user.click(within(confirm).getByRole('button', { name: 'Renew' }));
  expect(await screen.findByText('Token renewed · verified with the provider')).toBeDefined();
});

test('a rejected token shows on the screen, with Renew token to try again', async () => {
  const user = screenOf('staging');
  await user.click(await screen.findByRole('button', { name: 'Renew' }));
  await user.type(screen.getByLabelText('Hetzner Cloud token'), 'x'.repeat(64)); // the mock's 401
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await user.click(
    within(await screen.findByRole('dialog', { name: /Renew the API token/ })).getByRole('button', {
      name: 'Renew',
    }),
  );
  const panel = await screen.findByRole('alert');
  expect(panel.textContent).toContain('rejected the supplied token');
  await user.click(within(panel).getByRole('button', { name: 'Renew token' }));
  expect(screen.getByRole('dialog', { name: 'Renew API token' })).toBeDefined();
  expect(screen.queryByRole('alert')).toBeNull();
  // Review #14: the button that opened the form went with the panel. Closed, the form leaves
  // the focus on the page's heading — in this view, not on <body> (where Tab starts over).
  await user.click(screen.getByRole('button', { name: 'Cancel' }));
  expect(document.activeElement).toBe(screen.getByRole('heading', { level: 1, name: 'Target' }));
});

test('remove: the full plan, the typed name, then the gesture; the tab closes', async () => {
  const onRemoved = mock();
  const user = screenOf('prod-eu', { onRemoved });
  await user.click(await screen.findByRole('button', { name: 'Remove…' }));
  const confirm = await screen.findByRole('dialog', { name: 'Remove target prod-eu?' });
  expect(confirm.textContent).toContain('keeps running at the provider');
  const go = within(confirm).getByRole('button', { name: 'Remove target' }) as HTMLButtonElement;
  expect(go.disabled).toBe(true);
  await user.type(within(confirm).getByLabelText(/Type prod-eu to confirm/), 'prod-eu');
  await user.click(go);
  await waitFor(() => expect(onRemoved).toHaveBeenCalledWith('prod-eu'));
  expect(
    await screen.findByText(/Removed prod-eu from this computer · server prod-eu-1 keeps running/),
  ).toBeDefined();
});

test('make default from its row runs at once: no dialog, the row says Yes', async () => {
  const user = screenOf('staging');
  const row = await screen.findByRole('group', { name: 'CLI default' });
  expect(row.textContent).toContain('No');
  await user.click(within(row).getByRole('button', { name: 'Make default' }));
  expect(await screen.findByText('staging is the CLI default now')).toBeDefined();
  expect(screen.queryByRole('dialog')).toBeNull();
  await waitFor(() =>
    expect(screen.getByRole('group', { name: 'CLI default' }).textContent).toContain('Yes'),
  );
});

/** The arguments of every `cmd` the page sends from here on, read off the mock IPC. */
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

test('SSH key: Change offers the keys in ~/.ssh, the one in use disabled, and saves only the key', async () => {
  const renews = sentArgs('op_plan_target_renew');
  const user = screenOf('staging');
  const row = await screen.findByRole('group', { name: 'SSH key' });
  await user.click(within(row).getByRole('button', { name: 'Change' }));
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  const inUse = within(form).getByRole('radio', { name: '~/.ssh/id_ed25519.pub' });
  expect((inUse as HTMLInputElement).disabled).toBe(true);
  const other = within(form).getByRole('radio', { name: '~/.ssh/work.pub' }) as HTMLInputElement;
  expect(other.checked).toBe(true);
  expect(within(form).queryByLabelText('Hetzner Cloud token')).toBeNull(); // no token asked
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  const confirm = await screen.findByRole('dialog', { name: 'Change the SSH key of staging?' });
  expect(renews).toEqual([{ name: 'staging', token: null, sshKey: '/home/alex/.ssh/work.pub' }]);
  expect(confirm.textContent).toContain('ssh key: ~/.ssh/id_ed25519.pub → ~/.ssh/work.pub');
  expect(confirm.textContent).not.toContain('Credentials'); // the key alone
  await user.click(within(confirm).getByRole('button', { name: 'Change key' }));
  expect(await screen.findByText('SSH key changed · credentials unchanged')).toBeDefined();
  await waitFor(() =>
    expect(screen.getByRole('group', { name: 'SSH key' }).textContent).toContain('~/.ssh/work.pub'),
  );
});

test('SSH key: a key that became the one in use since the form opened is refused in the form', async () => {
  const user = screenOf('staging');
  await user.click(
    within(await screen.findByRole('group', { name: 'SSH key' })).getByRole('button', {
      name: 'Change',
    }),
  );
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  // Meanwhile (another window, the CLI): staging's key becomes work.pub, the form's choice.
  const meanwhile = await api.opPlanTargetRenew('staging', null, '/home/alex/.ssh/work.pub');
  await (await startPlan(meanwhile.opId)).ended;
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  expect((await within(form).findByRole('alert')).textContent).toContain(
    'renewing `staging` would change nothing: no new token and no new SSH key',
  );
  expect(screen.queryByRole('dialog', { name: /Change the SSH key of/ })).toBeNull();
});

test('SSH key: Other path… is looked up first; a path with no file says so in the form', async () => {
  const user = screenOf('lab'); // its stored key is missing
  const row = await screen.findByRole('group', { name: 'SSH key' });
  expect(row.textContent).toContain('(missing)');
  await user.click(within(row).getByRole('button', { name: 'Change' }));
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  // Nothing in ~/.ssh is in use by lab: the first key is chosen.
  expect(
    (within(form).getByRole('radio', { name: '~/.ssh/id_ed25519.pub' }) as HTMLInputElement)
      .checked,
  ).toBe(true);
  await user.click(within(form).getByRole('radio', { name: 'Other path…' }));
  await user.type(within(form).getByLabelText('Path to a public key'), '~/.ssh/nothing.pub');
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  expect((await within(form).findByRole('alert')).textContent).toContain(
    'No file at ~/.ssh/nothing.pub.',
  );
  await user.clear(within(form).getByLabelText('Path to a public key'));
  await user.type(within(form).getByLabelText('Path to a public key'), '~/.ssh/work.pub');
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  const confirm = await screen.findByRole('dialog', { name: 'Change the SSH key of lab?' });
  expect(confirm.textContent).toContain('ssh key: ~/.ssh/lab.pub → ~/.ssh/work.pub');
});

test('SSH key: Other path… refuses a private key by name, and a file that is no key (GOTCHA-149)', async () => {
  const renews = sentArgs('op_plan_target_renew');
  const user = screenOf('staging');
  await user.click(
    within(await screen.findByRole('group', { name: 'SSH key' })).getByRole('button', {
      name: 'Change',
    }),
  );
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  await user.click(within(form).getByRole('radio', { name: 'Other path…' }));
  const path = within(form).getByLabelText('Path to a public key');
  await user.type(path, '/home/alex/.ssh/id_ed25519');
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  expect((await within(form).findByRole('alert')).textContent).toContain(
    '~/.ssh/id_ed25519 is a private key: choose its public half, the .pub file next to it.',
  );
  await user.clear(path);
  await user.type(path, '/home/alex/notes.txt');
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  expect((await within(form).findByRole('alert')).textContent).toContain(
    '~/notes.txt is not an SSH public key.',
  );
  // Refused from the lookup: no plan was asked for.
  expect(renews).toEqual([]);
  expect(screen.queryByRole('dialog', { name: /Change the SSH key of/ })).toBeNull();
});

test('SSH key: Other path… refuses a relative path in the form: the full path, or ~/', async () => {
  const renews = sentArgs('op_plan_target_renew');
  const user = screenOf('staging');
  await user.click(
    within(await screen.findByRole('group', { name: 'SSH key' })).getByRole('button', {
      name: 'Change',
    }),
  );
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  await user.click(within(form).getByRole('radio', { name: 'Other path…' }));
  await user.type(within(form).getByLabelText('Path to a public key'), '.ssh/work.pub');
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  const alert = await within(form).findByRole('alert');
  expect(alert.textContent).toContain('`.ssh/work.pub` is not a full path');
  expect(alert.textContent).toContain('Give the full path, or start it with ~/');
  expect(renews).toEqual([]);
  expect(screen.queryByRole('dialog', { name: /Change the SSH key of/ })).toBeNull();
});

test('SSH key: a .pub in ~/.ssh that holds no public key is listed and cannot be chosen', async () => {
  const user = screenOf('staging');
  await user.click(
    within(await screen.findByRole('group', { name: 'SSH key' })).getByRole('button', {
      name: 'Change',
    }),
  );
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  const old = within(form).getByRole('radio', { name: '~/.ssh/old.pub' }) as HTMLInputElement;
  expect(old.disabled).toBe(true);
  expect(old.closest('label')?.textContent).toContain('not an SSH public key');
});

test('SSH key: the key in use typed as another path is refused in the form', async () => {
  const user = screenOf('staging');
  await user.click(
    within(await screen.findByRole('group', { name: 'SSH key' })).getByRole('button', {
      name: 'Change',
    }),
  );
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  await user.click(within(form).getByRole('radio', { name: 'Other path…' }));
  await user.type(within(form).getByLabelText('Path to a public key'), '~/.ssh/id_ed25519.pub');
  await user.click(within(form).getByRole('button', { name: 'Continue' }));
  expect((await within(form).findByRole('alert')).textContent).toContain(
    'staging uses that key now: choose another.',
  );
});

test('a target that cannot be shown says why, and its Danger zone still removes it', async () => {
  const onRemoved = mock();
  const user = screenOf('broken', { onRemoved });
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain('targets/broken/config.yaml: expected a mapping');
  // The neutral help offers the removal (WI-458), and the screen offers it too.
  expect(alert.textContent).toContain('Otherwise remove target `broken` and add it again');
  expect(screen.queryByRole('group', { name: 'Machine' })).toBeNull();
  await user.click(
    within(screen.getByRole('region', { name: 'Danger zone' })).getByRole('button', {
      name: 'Remove…',
    }),
  );
  const confirm = await screen.findByRole('dialog', { name: 'Remove target broken?' });
  expect(confirm.textContent).toContain('config.yaml cannot be read: expected a mapping');
  await user.type(within(confirm).getByLabelText(/Type broken to confirm/), 'broken');
  await user.click(within(confirm).getByRole('button', { name: 'Remove target' }));
  await waitFor(() => expect(onRemoved).toHaveBeenCalledWith('broken'));
});

// Review #6: a file of the target's own that cannot be read for an I/O error (its permissions)
// names the target as a parse error does, so the Danger zone removes it the same way.
test('an I/O error on a file of the target offers its removal too', async () => {
  const internals = (
    window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown, options?: unknown) => unknown };
    }
  ).__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  internals.invoke = (cmd, args, options) =>
    cmd === 'target_show'
      ? Promise.reject(
          uiError(CORE_ERROR_CODES.IO_ERROR, 'io error: Permission denied (os error 13)', {
            path: '~/.config/apprafter/targets/lab/credentials.yaml',
            target: 'lab',
          }),
        )
      : invoke(cmd, args, options);
  const onRemoved = mock();
  const user = screenOf('lab', { onRemoved });
  expect((await screen.findByRole('alert')).textContent).toContain('Permission denied');
  await user.click(
    within(screen.getByRole('region', { name: 'Danger zone' })).getByRole('button', {
      name: 'Remove…',
    }),
  );
  const confirm = await screen.findByRole('dialog', { name: 'Remove target lab?' });
  await user.type(within(confirm).getByLabelText(/Type lab to confirm/), 'lab');
  await user.click(within(confirm).getByRole('button', { name: 'Remove target' }));
  await waitFor(() => expect(onRemoved).toHaveBeenCalledWith('lab'));
});

// The store's own config.yaml belongs to no target: removing this one would not repair it (and
// its plan is refused with the same error), so no Danger zone is offered; nor for an error on
// another target's file, or one that names no target at all — parse error and I/O error alike.
test('a target that cannot be shown for another reason offers no removal', async () => {
  for (const code of [CORE_ERROR_CODES.TARGET_INVALID_CONFIG, CORE_ERROR_CODES.IO_ERROR]) {
    for (const fields of [{ path: '~/.config/apprafter/config.yaml' }, { target: 'lab' }, {}]) {
      clearMocks();
      mockIPC((cmd) =>
        cmd === 'target_show' ? Promise.reject(uiError(code, 'cannot be read', fields)) : null,
      );
      screenOf('staging');
      expect((await screen.findByRole('alert')).textContent).toContain('cannot be read');
      expect(screen.queryByRole('region', { name: 'Danger zone' })).toBeNull();
      cleanup();
    }
  }
});

test('Run doctor in the page header opens Doctor · <name>', async () => {
  const user = screenOf('prod-eu');
  await user.click(await screen.findByRole('button', { name: 'Run doctor' }));
  expect(await screen.findByRole('dialog', { name: 'Doctor · prod-eu' })).toBeDefined();
});

test("the Machine row's Change opens Change machine · <name> for a target with no server", async () => {
  const user = screenOf('staging');
  const machine = await screen.findByRole('group', { name: 'Machine' });
  await user.click(within(machine).getByRole('button', { name: 'Change' }));
  const dialog = await screen.findByRole('dialog', { name: 'Change machine · staging' });
  // It opens on the machine the target is set to (its report: fsn1, cx22).
  await waitFor(() =>
    expect((within(dialog).getByRole('radio', { name: 'cx22' }) as HTMLInputElement).checked).toBe(
      true,
    ),
  );
});

test("a provisioned target has no Change, and D.3d's refusal text stays", async () => {
  screenOf('prod-eu');
  const machine = await screen.findByRole('group', { name: 'Machine' });
  expect(within(machine).queryByRole('button', { name: 'Change' })).toBeNull();
  expect(machine.textContent).toContain('apprafter backup create');
});

// D.3e review #12: the app's own overlays (the toolchain, the wizard) open beside the views, so
// the view's heading is not their host's child. Opened by an ErrorPanel action, which goes with
// the panel, they closed with the focus on <body>, where the next Tab starts over from the title
// bar. Their fallback is the heading of the view that is shown.
for (const [code, action, dialog] of [
  ['apprafter::env::tool_not_found', 'Show the toolchain', 'Toolchain'],
  ['apprafter::target::not_found', 'Add a target', 'Add target'],
] as const) {
  test(`the panel's ${action}: closed, the focus goes to the page's heading`, async () => {
    const internals = (
      window as unknown as { __TAURI_INTERNALS__: { invoke: (...args: unknown[]) => unknown } }
    ).__TAURI_INTERNALS__;
    const invoke = internals.invoke;
    internals.invoke = (...args: unknown[]) =>
      args[0] === 'op_plan_target_use'
        ? Promise.reject(uiError(code))
        : invoke.apply(internals, args);
    const user = screenOf('staging');
    const row = await screen.findByRole('group', { name: 'CLI default' });
    await user.click(within(row).getByRole('button', { name: 'Make default' }));
    const panel = await screen.findByRole('alert');
    await user.click(within(panel).getByRole('button', { name: action }));
    const opened = await screen.findByRole('dialog', { name: dialog });
    expect(opened.contains(document.activeElement)).toBe(true);
    await user.keyboard('{Escape}');
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(screen.getByRole('heading', { level: 1, name: 'Target' }));
  });
}

/** Two tabs as the Shell hosts them: prod-eu's hidden before staging's, the app's overlays beside. */
function TwoTabs() {
  const app = useAppOverlayHost();
  return (
    <AppOverlayContext value={app.show}>
      <Activity mode="hidden">
        <ViewFrame>
          <TargetScreen name="prod-eu" onRenamed={() => {}} onRemoved={() => {}} />
        </ViewFrame>
      </Activity>
      <Activity mode="visible">
        <ViewFrame>
          <TargetScreen name="staging" onRenamed={() => {}} onRemoved={() => {}} />
        </ViewFrame>
      </Activity>
      {app.overlays}
    </AppOverlayContext>
  );
}

test("with a tab hidden before it, the fallback is the shown tab's heading", async () => {
  const internals = (
    window as unknown as { __TAURI_INTERNALS__: { invoke: (...args: unknown[]) => unknown } }
  ).__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  internals.invoke = (...args: unknown[]) =>
    args[0] === 'op_plan_target_use'
      ? Promise.reject(uiError('apprafter::env::tool_not_found'))
      : invoke.apply(internals, args);
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo()}>
        <ToastProvider>
          <TwoTabs />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  const user = userEvent.setup();
  const row = await screen.findByRole('group', { name: 'CLI default' });
  const view = row.closest('.view');
  await user.click(within(row).getByRole('button', { name: 'Make default' }));
  const panel = await screen.findByRole('alert');
  await user.click(within(panel).getByRole('button', { name: 'Show the toolchain' }));
  await screen.findByRole('dialog', { name: 'Toolchain' });
  await user.keyboard('{Escape}');
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  expect(document.activeElement?.tagName).toBe('H1');
  expect(view?.contains(document.activeElement)).toBe(true);
});

test("the doctor's Add a target: the wizard it opens closes onto the page's heading", async () => {
  const user = screenOf('staging');
  await screen.findByRole('group', { name: 'CLI default' });
  // The target goes behind the screen's back (the CLI removed it): its doctor offers to add it.
  const removal = await api.opPlanTargetRemove('staging');
  await (await startPlan(removal.opId)).ended;
  // Clicked as WKWebView clicks a button, which never takes the focus: no opener to go back to.
  fireEvent.click(screen.getByRole('button', { name: 'Run doctor' }));
  const doctor = await screen.findByRole('dialog', { name: 'Doctor · staging' });
  fireEvent.click(await within(doctor).findByRole('button', { name: 'Add a target' }));
  const wizard = await screen.findByRole('dialog', { name: 'Add target' });
  expect(wizard.contains(document.activeElement)).toBe(true);
  await user.keyboard('{Escape}');
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  expect(document.activeElement).toBe(screen.getByRole('heading', { level: 1, name: 'Target' }));
});

// D.3e review #13: a view's form whose reads come back after the doctor opened over the view
// opens under the doctor, in the view the doctor made inert, and gets no focus there (happy-dom,
// as every engine, focuses nothing under [inert]). Closing the doctor must hand it the focus: the
// doctor's opener sits behind the form now.
test('a form that opened under the doctor gets the focus when the doctor closes; Esc closes it', async () => {
  const internals = (
    window as unknown as { __TAURI_INTERNALS__: { invoke: (...args: unknown[]) => unknown } }
  ).__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  let release = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  internals.invoke = async (...args: unknown[]) => {
    if (args[0] === 'ssh_key_candidates') await held;
    return invoke.apply(internals, args);
  };
  const user = screenOf('staging');
  const row = await screen.findByRole('group', { name: 'SSH key' });
  await user.click(within(row).getByRole('button', { name: 'Change' }));
  await user.click(screen.getByRole('button', { name: 'Run doctor' }));
  const doctor = await screen.findByRole('dialog', { name: 'Doctor · staging' });
  expect(doctor.contains(document.activeElement)).toBe(true);
  release();
  const form = await screen.findByRole('dialog', { name: 'Change SSH key' });
  expect(form.closest('[inert]')).not.toBeNull();
  expect(doctor.contains(document.activeElement)).toBe(true);
  await user.keyboard('{Escape}');
  await waitFor(() => expect(screen.queryByRole('dialog', { name: /Doctor/ })).toBeNull());
  expect(form.closest('[inert]')).toBeNull();
  expect(form.contains(document.activeElement)).toBe(true);
  await user.keyboard('{Escape}');
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  expect(document.activeElement).toBe(screen.getByRole('heading', { level: 1, name: 'Target' }));
});
