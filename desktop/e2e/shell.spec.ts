// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One walk through the shell on the mock IPC: the lock screen, Unlock, the Targets view, a
// target's every section (the Target section a screen, the rest planned), Settings and its theme
// switch, and Mod+L back to the lock screen.
import { expect, type Page, type TestInfo, test } from '@playwright/test';

const SECTIONS = [
  'Overview',
  'Applications',
  'Approvals',
  'Backups',
  'Data',
  'Networking',
  'Platform',
  'Nodes',
  'Target',
];

async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(name, { body: await page.screenshot(), contentType: 'image/png' });
}

test('the shell, from the lock screen to the lock again', async ({ page }, info) => {
  const query = String(info.project.metadata.query ?? '');
  const windows = query.includes('os=windows');
  // The mock is a Mac unless told otherwise: Mod is Cmd there, Ctrl on Windows.
  const mod = windows ? 'Control' : 'Meta';

  await page.goto(`/${query}`);
  await expect(page.getByRole('heading', { name: 'AppRafter is locked' })).toBeVisible();
  if (windows) {
    for (const name of ['Minimize', 'Maximize', 'Close']) {
      await expect(page.getByRole('button', { name, exact: true })).toBeVisible();
    }
  }
  await shot(page, info, 'lock screen');

  await page.getByRole('button', { name: 'Unlock' }).click();
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
  await shot(page, info, 'targets');

  await page.getByRole('button', { name: 'Open prod-eu' }).click();
  await expect(page.getByRole('tab', { name: 'prod-eu' })).toHaveAttribute('aria-selected', 'true');
  for (const section of SECTIONS) {
    await page.getByRole('button', { name: section, exact: true }).click();
    await expect(page.getByRole('heading', { level: 1, name: section })).toBeVisible();
    if (section === 'Target') {
      // The Target section is a screen of its own (D.3): the target's card.
      await expect(page.getByRole('region', { name: 'Target' })).toBeVisible();
    } else {
      await expect(page.getByRole('heading', { name: /arrives? in D\.\d+\.$/ })).toBeVisible();
      if (section === 'Nodes') await shot(page, info, 'a planned section');
    }
  }
  await shot(page, info, 'the Target screen');

  await page.getByRole('button', { name: /^Settings/ }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings).toBeVisible();
  const html = page.locator('html');
  const other = (await html.getAttribute('data-theme')) === 'light' ? 'dark' : 'light';
  await settings.getByRole('radio', { name: other === 'light' ? 'Light' : 'Dark' }).click();
  await expect(html).toHaveAttribute('data-theme', other);
  await shot(page, info, `settings, switched to ${other}`);
  await page.keyboard.press('Escape');
  await expect(settings).toBeHidden();

  await page.keyboard.press(`${mod}+KeyL`);
  await expect(page.getByRole('heading', { name: 'AppRafter is locked' })).toBeVisible();
  await shot(page, info, 'locked again');
});
