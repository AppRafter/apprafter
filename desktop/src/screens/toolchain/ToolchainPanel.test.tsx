// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { QueryClientProvider } from '@tanstack/react-query';
import { clearMocks } from '@tauri-apps/api/mocks';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { ToastProvider, ToastViewport } from '../../components/Toast';
import { resetOperations } from '../../ipc/operations';
import { ViewFrame } from '../../shell/ViewFrame';
import { PlatformContext } from '../../state/platform';
import { createQueryClient } from '../../state/queryClient';
import { appInfo } from '../../test/fixtures';
import { toolchainReport } from '../../test/flows';
import { type Harness, installHarness, uiError } from '../../test/ipc';
import { settleIpc } from '../../test/settle';
import { ToolchainPanel } from './ToolchainPanel';

let h: Harness;
beforeEach(() => {
  h = installHarness();
  h.answer('toolchain_status', toolchainReport());
});
afterEach(async () => {
  cleanup();
  await settleIpc();
  resetOperations();
  clearMocks();
});

function renderPanel(os: 'linux' | 'macos' | 'windows') {
  const onClose = mock();
  render(
    <QueryClientProvider client={createQueryClient()}>
      <PlatformContext value={appInfo({ os })}>
        <ToastProvider>
          <ViewFrame>
            <ToolchainPanel onClose={onClose} />
          </ViewFrame>
          <ToastViewport />
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>,
  );
  return { user: userEvent.setup(), onClose };
}

/** The row of one tool, by its name. */
const toolRow = (tool: string) => {
  const name = screen.getAllByText(tool).find((e) => e.classList.contains('tool-name'));
  const row = name?.closest('li');
  if (!row) throw new Error(`no row for ${tool}`);
  return row;
};

test("macOS: a found tool's version and path; a missing one's macOS line; a note is not a command", async () => {
  renderPanel('macos');
  expect(screen.getByRole('dialog', { name: 'Toolchain' })).toBeDefined();
  expect(await screen.findByText('Client Version: v1.34.1')).toBeDefined();
  expect(screen.getByText('/usr/bin/kubectl')).toBeDefined();
  expect(screen.getByText('Not found on the search path')).toBeDefined();
  expect(screen.getByText('brew install helm').tagName).toBe('CODE');
  expect(screen.queryByText(/winget/)).toBeNull();
  // ssh timed out, so its install line shows; on macOS it reads "preinstalled": a note to read.
  expect(screen.getByText('preinstalled').tagName).not.toBe('CODE');
});

test("Windows: the missing tool's winget line", async () => {
  renderPanel('windows');
  expect(await screen.findByText('winget install Helm.Helm')).toBeDefined();
  expect(screen.queryByText('brew install helm')).toBeNull();
});

test("Linux: every distribution's line, each labelled; a link is shown as its address", async () => {
  renderPanel('linux');
  await screen.findByText('Client Version: v1.34.1');
  const helm = toolRow('helm');
  for (const label of ['Debian / Ubuntu', 'Nix', 'Other']) {
    expect(within(helm).getByText(label)).toBeDefined();
  }
  expect(within(helm).getByText('apt install helm').tagName).toBe('CODE');
  // The opener may open only the app's own three URLs, so an install page is shown, not linked.
  const link = within(helm).getByText('https://helm.sh/docs/intro/install/');
  expect(link.tagName).toBe('CODE');
  expect(link.closest('a')).toBeNull();
  expect(within(helm).queryByText(/brew|winget/)).toBeNull();
});

test('a found tool shows no install lines; required or optional, and the state tone', async () => {
  renderPanel('macos');
  await screen.findByText('Client Version: v1.34.1');
  const kubectl = toolRow('kubectl');
  expect(within(kubectl).getByText('required')).toBeDefined();
  expect(within(kubectl).queryByText('brew install kubectl')).toBeNull();
  expect(within(kubectl).getByText('Client Version: v1.34.1').getAttribute('data-tone')).toBe('ok');
  const helm = toolRow('helm');
  expect(within(helm).getByText('optional')).toBeDefined();
  // An optional tool that is missing is a warning, not an error.
  expect(within(helm).getByText('Not found on the search path').getAttribute('data-tone')).toBe(
    'warn',
  );
});

test("a probe's own words are shown as they are", async () => {
  renderPanel('macos');
  expect(
    await screen.findByText(
      'xcrun: error: invalid active developer path (/Library/Developer/CommandLineTools)',
    ),
  ).toBeDefined();
  expect(screen.getByText('Found, but it exited 1 without printing a version')).toBeDefined();
});

test('Check again reads the toolchain again', async () => {
  const { user } = renderPanel('macos');
  await screen.findByText('Client Version: v1.34.1');
  await user.click(screen.getByRole('button', { name: 'Check again' }));
  await waitFor(() => expect(h.of('toolchain_status')).toHaveLength(2));
});

test('Check again by keyboard keeps the focus while it reads: a press starts nothing more, Esc closes', async () => {
  const { user, onClose } = renderPanel('macos');
  await screen.findByText('Client Version: v1.34.1');
  let answer = (_report: unknown) => {};
  h.answer(
    'toolchain_status',
    () =>
      new Promise((resolve) => {
        answer = resolve;
      }),
  );
  const again = screen.getByRole('button', { name: 'Check again' }) as HTMLButtonElement;
  again.focus();
  await user.keyboard('{Enter}');
  await waitFor(() => expect(h.of('toolchain_status')).toHaveLength(2));
  // Waiting, not disabled: a disabled button drops the focus onto the page (review #1).
  expect(again.getAttribute('aria-disabled')).toBe('true');
  expect(again.disabled).toBe(false);
  expect(document.activeElement).toBe(again);
  await user.keyboard('{Enter}');
  expect(h.of('toolchain_status')).toHaveLength(2);
  answer(toolchainReport());
  await waitFor(() => expect(again.getAttribute('aria-disabled')).toBeNull());
  await user.keyboard('{Escape}');
  expect(onClose).toHaveBeenCalledTimes(1);
});

test('reading again over the rows says so: the rows are marked as the last check until it ends', async () => {
  const { user } = renderPanel('macos');
  await screen.findByText('Client Version: v1.34.1');
  let answer = (_report: unknown) => {};
  h.answer(
    'toolchain_status',
    () =>
      new Promise((resolve) => {
        answer = resolve;
      }),
  );
  await user.click(screen.getByRole('button', { name: 'Check again' }));
  expect(
    await screen.findByText('Checking the tools again. These lines are from the last check.'),
  ).toBeDefined();
  expect(document.querySelector('.tool-rows')?.getAttribute('aria-busy')).toBe('true');
  answer(toolchainReport());
  // A boolean, not the element: bun formats a failing element match slowly enough to starve
  // waitFor's loop.
  await waitFor(() => expect(screen.queryByText(/^Checking the tools again/) === null).toBe(true));
  expect(document.querySelector('.tool-rows')?.getAttribute('aria-busy')).toBeNull();
});

test('opened again while the last check is cached: it says the lines are from the last check', async () => {
  const client = createQueryClient();
  const panel = () => (
    <QueryClientProvider client={client}>
      <PlatformContext value={appInfo({ os: 'macos' })}>
        <ToastProvider>
          <ViewFrame>
            <ToolchainPanel onClose={() => {}} />
          </ViewFrame>
        </ToastProvider>
      </PlatformContext>
    </QueryClientProvider>
  );
  const first = render(panel());
  await screen.findByText('Client Version: v1.34.1');
  first.unmount();
  h.answer('toolchain_status', () => new Promise(() => {})); // the new check keeps running
  render(panel());
  expect(
    await screen.findByText('Checking the tools again. These lines are from the last check.'),
  ).toBeDefined();
  expect(screen.getByText('Client Version: v1.34.1')).toBeDefined();
});

test('a check again that is refused shows why, and no footer from the check before', async () => {
  const { user } = renderPanel('macos');
  await screen.findByText('Searched 2 directories of the PATH your login shell sets');
  h.answer('toolchain_status', () =>
    Promise.reject(uiError('apprafter::desktop::internal', 'the probe broke')),
  );
  await user.click(screen.getByRole('button', { name: 'Check again' }));
  expect(await screen.findByText('the probe broke')).toBeDefined();
  expect(screen.queryByText(/^Searched /)).toBeNull();
});

test('the footer names the search path and, opened, lists it', async () => {
  const { user } = renderPanel('macos');
  const summary = await screen.findByText(
    'Searched 2 directories of the PATH your login shell sets',
  );
  await user.click(summary);
  expect(screen.getByText('/usr/local/bin')).toBeDefined();
  expect(screen.getByText('/usr/bin')).toBeDefined();
});

test('while it reads it says so; a refusal is shown with Check again', async () => {
  let answer = (_value: unknown) => {};
  h.answer(
    'toolchain_status',
    () =>
      new Promise((resolve) => {
        answer = resolve;
      }),
  );
  const { user } = renderPanel('macos');
  expect(await screen.findByText('Checking the tools…')).toBeDefined();
  answer(Promise.reject(uiError('apprafter::desktop::internal', 'the probe broke')));
  expect(await screen.findByText('the probe broke')).toBeDefined();
  h.answer('toolchain_status', toolchainReport());
  await user.click(screen.getByRole('button', { name: 'Check again' }));
  expect(await screen.findByText('Client Version: v1.34.1')).toBeDefined();
});
