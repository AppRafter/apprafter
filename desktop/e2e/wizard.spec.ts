// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The add-target wizard and Change machine on the mock IPC, in every project of the config
// (chromium and webkit, dark and light, and Windows chrome): a rejected token, a verified one that
// leaves the page, the machine table's sort, filters and sold-out reveal, the keyboard on its rows,
// the details (a private key refused by name, a missing file), Save; the wizard over every view
// with the tab strip inert; a lock that takes the wizard with it.
import { expect, type Page, type TestInfo, test } from '@playwright/test';

const TOKEN = 'A1'.repeat(32);
// The mock's 401: D.3d's rule (a well-formed token starting with x), built as flows.ts builds it.
const REJECTED = 'x'.repeat(64);

async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(name, { body: await page.screenshot(), contentType: 'image/png' });
}

async function unlocked(page: Page, info: TestInfo) {
  await page.goto(`/${String(info.project.metadata.query ?? '')}`);
  await page.getByRole('button', { name: 'Unlock' }).click();
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
}

/**
 * Opens `name`'s tab from the Targets view through D.3d's TargetCard button, whose accessible name
 * is its aria-label `Open <name>`. Every walk starts from a fresh unlock, so the tab is never
 * already open (which would make it `Switch to <name>`).
 */
async function openTarget(page: Page, name: string) {
  await page.getByRole('button', { name: `Open ${name}`, exact: true }).click();
  await expect(page.getByRole('tab', { name })).toHaveAttribute('aria-selected', 'true');
}

test('add a target: token, machine, details, saved', async ({ page }, info) => {
  await unlocked(page, info);
  await page.getByRole('button', { name: /Add target/ }).click();
  const wizard = page.getByRole('dialog', { name: 'Add target' });
  // The app's own overlay: the tab strip under it is inert.
  await expect(page.locator('.tabstrip')).toHaveAttribute('inert', '');
  await expect(wizard.getByRole('radio', { name: /Hetzner Cloud/ })).toBeChecked();
  const field = wizard.getByRole('textbox', { name: 'API token' });
  await field.fill(REJECTED);
  await wizard.getByRole('button', { name: 'Verify and continue' }).click();
  await expect(wizard.getByRole('alert')).toContainText('rejected the supplied token');
  await field.fill(TOKEN);
  await shot(page, info, 'provider step');
  await wizard.getByRole('button', { name: 'Verify and continue' }).click();

  const table = wizard.getByRole('table', { name: 'Machines in nbg1' });
  await expect(table).toBeVisible();
  expect(await page.content()).not.toContain(TOKEN);
  // The recommended type is chosen; the cheapest first.
  await expect(table.getByRole('radio', { name: 'cx32' })).toBeChecked();
  const monthly = table.getByRole('columnheader', { name: /€ \/ mo/ });
  await expect(monthly).toHaveAttribute('aria-sort', 'ascending');
  await monthly.getByRole('button').click();
  await expect(monthly).toHaveAttribute('aria-sort', 'descending');
  await monthly.getByRole('button').click();
  const arch = wizard.getByRole('radiogroup', { name: 'Architecture' });
  await arch.getByRole('radio', { name: 'Arm' }).click();
  await expect(table.getByRole('radio', { name: 'cx22' })).toHaveCount(0);
  await expect(table.getByRole('radio', { name: 'cax11' })).toBeVisible();
  await arch.getByRole('radio', { name: 'All' }).click();
  // Sold out and retired are hidden until asked for, then shown and not choosable.
  await expect(table.getByRole('radio', { name: 'cpx31' })).toHaveCount(0);
  await wizard.getByRole('button', { name: /^Show \d+ unavailable$/ }).click();
  await expect(table.getByRole('radio', { name: 'cpx31' })).toBeDisabled();
  await expect(table.getByRole('radio', { name: 'cx11' })).toBeDisabled();
  await table.getByRole('radio', { name: 'cx22' }).check();
  await page.keyboard.press('ArrowDown'); // the engine moves the choice, past disabled rows
  await expect(table.getByRole('radio', { name: 'cx22' })).not.toBeChecked();
  await shot(page, info, 'machine step, unavailable shown');
  await wizard.getByText('Helsinki').click();
  await expect(wizard.getByRole('table', { name: 'Machines in hel1' })).toBeVisible();
  await shot(page, info, 'machine step');
  await wizard.getByRole('button', { name: 'Continue' }).click();

  await wizard.getByLabel('Target name').fill('lab-2');
  // A card's label covers its native radio: the owner clicks the card.
  await wizard.getByText('Team (T2)', { exact: true }).click();
  await expect(wizard.getByRole('radio', { name: /Team \(T2\)/ })).toBeChecked();
  await expect(wizard.getByRole('radio', { name: '~/.ssh/id_ed25519.pub' })).toBeChecked();
  await wizard.getByText('Other path…', { exact: true }).click();
  await expect(wizard.getByRole('radio', { name: 'Other path…' })).toBeChecked();
  const path = wizard.getByLabel('Path to a public key');
  await path.fill('~/.ssh/id_ed25519');
  await path.blur();
  await expect(wizard.getByText(/is a private key: choose its public half/)).toBeVisible();
  await expect(wizard.getByRole('button', { name: 'Save target' })).toBeDisabled();
  await path.fill('/nowhere/key');
  await path.blur();
  await expect(wizard.getByText('No file at /nowhere/key.')).toBeVisible();
  await expect(wizard.getByRole('button', { name: 'Save target' })).toBeDisabled();
  await wizard.getByText('Skip', { exact: true }).click();
  await expect(wizard.getByRole('radio', { name: 'Skip' })).toBeChecked();
  await shot(page, info, 'details step');
  await wizard.getByRole('button', { name: 'Save target' }).click();
  await expect(wizard).toBeHidden();
  await expect(page.getByRole('status')).toContainText('Target “lab-2” saved.');
  await expect(page.locator('.tabstrip')).not.toHaveAttribute('inert', '');
  await expect(page.getByRole('article', { name: 'lab-2' })).toBeVisible();
});

test('a lock takes the wizard and what was typed', async ({ page }, info) => {
  const mod = String(info.project.metadata.query ?? '').includes('os=windows') ? 'Control' : 'Meta';
  await unlocked(page, info);
  await page.getByRole('button', { name: /Add target/ }).click();
  await page.getByRole('textbox', { name: 'API token' }).fill(TOKEN);
  await page.keyboard.press(`${mod}+KeyL`);
  await expect(page.getByRole('heading', { name: 'AppRafter is locked' })).toBeVisible();
  expect(await page.content()).not.toContain(TOKEN);
  await page.getByRole('button', { name: 'Unlock' }).click();
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
  await expect(page.getByRole('dialog', { name: 'Add target' })).toHaveCount(0);
});

test('change the machine of a target with no server', async ({ page }, info) => {
  await unlocked(page, info);
  await openTarget(page, 'staging');
  await page.getByRole('button', { name: 'Target', exact: true }).click();
  await page
    .getByRole('group', { name: 'Machine' })
    .getByRole('button', { name: 'Change', exact: true })
    .click();
  const dialog = page.getByRole('dialog', { name: 'Change machine · staging' });
  // staging is fsn1 / cx22 (D.3d's fixture); cx32 is the catalogue's recommended type.
  await expect(dialog.getByRole('table', { name: 'Machines in fsn1' })).toBeVisible();
  await expect(dialog.getByRole('radio', { name: 'cx22' })).toBeChecked();
  await dialog.getByRole('radio', { name: 'cx32' }).check();
  await shot(page, info, 'change machine');
  await dialog.getByRole('button', { name: 'Apply machine' }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole('status')).toContainText('Machine for “staging”: cx32 in fsn1.');
});
