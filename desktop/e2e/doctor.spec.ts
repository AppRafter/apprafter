// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Doctor and About › This computer on the mock IPC, in every project of the config: a run from
// the sidebar's stethoscope over every view (the tab strip inert), its chips and groups, Copy
// report into the mock clipboard, the toolchain a missing tool's fix opens above it (winget on
// the Windows project, brew elsewhere: the mock is a Mac unless ?os=), Escape closing one layer at
// a time; an SSH key fix opening the key change above the doctor; then This computer's rows,
// Verify and the toolchain.
import { expect, type Page, type TestInfo, test } from '@playwright/test';

// Copied from wizard.spec.ts (Playwright specs share no module here).
async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(name, { body: await page.screenshot(), contentType: 'image/png' });
}

async function unlocked(page: Page, info: TestInfo) {
  await page.goto(`/${String(info.project.metadata.query ?? '')}`);
  await page.getByRole('button', { name: 'Unlock' }).click();
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
}

/**
 * The words of the commands in `scope` that a line break splits ("--" at a line's end, "target"
 * on the next): each word's text measured with a Range, whatever wraps it.
 */
function splitWords(scope: ReturnType<Page['locator']>): Promise<string[]> {
  return scope.locator('code').evaluateAll((codes) =>
    codes.flatMap((code) => {
      const text = code.textContent ?? '';
      const split: string[] = [];
      const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
      const nodes: Text[] = [];
      while (walker.nextNode()) nodes.push(walker.currentNode as Text);
      for (const node of nodes) {
        for (const match of (node.textContent ?? '').matchAll(/\S+/g)) {
          const range = document.createRange();
          range.setStart(node, match.index);
          range.setEnd(node, match.index + match[0].length);
          const tops = new Set([...range.getClientRects()].map((r) => Math.round(r.top)));
          if (tops.size > 1) split.push(`${match[0]} (in ${text})`);
        }
      }
      return split;
    }),
  );
}

/** D.3d's TargetCard button: accessible name `Open <name>` (its aria-label). */
async function openTarget(page: Page, name: string) {
  await page.getByRole('button', { name: `Open ${name}`, exact: true }).click();
  await expect(page.getByRole('tab', { name })).toHaveAttribute('aria-selected', 'true');
}

test('doctor: groups, chips, copy report, the toolchain', async ({ page }, info) => {
  const windows = String(info.project.metadata.query ?? '').includes('os=windows');
  await unlocked(page, info);
  await openTarget(page, 'prod-eu');
  await page.locator('.cluster-header').getByRole('button', { name: 'Run doctor' }).click();
  const doctor = page.getByRole('dialog', { name: 'Doctor · prod-eu' });
  await expect(page.locator('.tabstrip')).toHaveAttribute('inert', '');
  for (const chip of ['pass', 'warn', 'fail', 'skipped']) {
    await expect(doctor.getByText(new RegExp(`^\\d+ ${chip}$`))).toBeVisible();
  }
  await expect(doctor.getByRole('heading', { level: 3 })).toHaveText([
    'Target',
    'Cluster',
    'This computer',
  ]);
  // A command in a fix line wraps between its words, never inside one, at whatever width the
  // dialog has: swept narrower, every word of every command comes to a line's end somewhere.
  expect(await doctor.locator('code').count()).toBeGreaterThan(0);
  const width = await doctor.evaluate((panel) => (panel as HTMLElement).style.width);
  const split = new Set<string>();
  for (let px = 340; px <= 660; px += 8) {
    await doctor.evaluate((panel, w) => {
      (panel as HTMLElement).style.width = `${w}px`;
    }, px);
    for (const word of await splitWords(doctor)) split.add(word);
  }
  await doctor.evaluate((panel, w) => {
    (panel as HTMLElement).style.width = w;
  }, width);
  expect([...split]).toEqual([]);
  await doctor.getByRole('button', { name: 'Copy report' }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { __mockClipboard?: string }).__mockClipboard ?? '',
      ),
    )
    .toContain('AppRafter doctor · prod-eu');
  await expect(page.getByRole('status')).toContainText('Report copied');
  await shot(page, info, 'doctor');

  await doctor.getByRole('button', { name: 'Show the toolchain' }).first().click();
  const toolchain = page.getByRole('dialog', { name: 'Toolchain' });
  await expect(
    toolchain.getByText(windows ? 'winget install Helm.Helm' : 'brew install helm'),
  ).toBeVisible();
  await expect(
    toolchain.getByText(windows ? 'brew install helm' : 'winget install Helm.Helm'),
  ).toHaveCount(0);
  await shot(page, info, 'toolchain over the doctor');
  await page.keyboard.press('Escape');
  await expect(toolchain).toBeHidden();
  await expect(doctor).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(doctor).toBeHidden();
  await expect(page.locator('.tabstrip')).not.toHaveAttribute('inert', '');
});

test("doctor: lab's missing key file opens the key change above the doctor", async ({
  page,
}, info) => {
  await unlocked(page, info);
  await openTarget(page, 'lab');
  await page.locator('.cluster-header').getByRole('button', { name: 'Run doctor' }).click();
  const doctor = page.getByRole('dialog', { name: 'Doctor · lab' });
  await doctor.getByRole('button', { name: 'Change SSH key' }).click();
  const form = page.getByRole('dialog', { name: 'Change SSH key' });
  await expect(form).toBeVisible();
  await shot(page, info, 'key change over the doctor');
  await form.getByRole('button', { name: 'Cancel' }).click();
  await expect(form).toBeHidden();
  await expect(doctor).toBeVisible();
});

test('About › This computer: identity, CLI default, Verify, the toolchain', async ({
  page,
}, info) => {
  await unlocked(page, info);
  await page.getByRole('button', { name: /^Settings/ }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings.getByText('anonymous (self-hosted)')).toBeVisible();
  await expect(settings.getByText(/^CLI default: prod-eu/)).toBeVisible();
  await expect(settings.getByText('Not checked yet')).toBeVisible();
  await settings.getByRole('button', { name: 'Verify' }).click();
  await expect(settings.getByText(/^Token verified/)).toBeVisible();
  await settings.getByRole('button', { name: 'Show', exact: true }).click();
  await expect(page.getByRole('dialog', { name: 'Toolchain' })).toBeVisible();
  await shot(page, info, 'this computer and the toolchain');
});
