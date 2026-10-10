// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The target screens on the mock IPC: the Targets page (an unreadable target shown, Make default),
// then a target's Target screen — each row's label and value on one line, at the default and
// the smallest window; rename (the tab follows), renew the token, change the SSH key alone,
// remove (the typed name, the gesture; the tab closes).
import { expect, type Locator, type Page, type TestInfo, test } from '@playwright/test';

async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(name, { body: await page.screenshot(), contentType: 'image/png' });
}

/**
 * The rows inside `scope` (by label) that are not laid out on one line: the label runs over more
 * than one line or past its own box, or the value has dropped below the label instead of sitting
 * beside it.
 */
function crampedRows(scope: Locator): Promise<string[]> {
  return scope.locator('.row').evaluateAll((rows) =>
    rows
      .filter((row) => {
        const label = row.querySelector('.row-label');
        const text = row.querySelector('.row-text');
        const value = row.querySelector('.row-value');
        if (label === null || text === null) return true;
        const range = document.createRange();
        range.selectNodeContents(label);
        const lines = new Set([...range.getClientRects()].map((rect) => Math.round(rect.top)));
        const overflows =
          range.getBoundingClientRect().right > label.getBoundingClientRect().right + 0.5;
        const below =
          value !== null &&
          value.getBoundingClientRect().left < text.getBoundingClientRect().right - 0.5;
        return lines.size > 1 || overflows || below;
      })
      .map((row) => row.querySelector('.row-label')?.textContent ?? ''),
  );
}

test('the Targets page and a target, from the list to removing it', async ({ page }, info) => {
  await page.goto(`/${String(info.project.metadata.query ?? '')}`);
  await page.getByRole('button', { name: 'Unlock' }).click();
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
  await expect(
    page.getByRole('article', { name: 'prod-eu' }).getByText('CLI default'),
  ).toBeVisible();
  await expect(page.getByRole('article', { name: 'broken' })).toContainText('Cannot be read');
  await shot(page, info, 'targets');

  await page.getByRole('button', { name: 'Make staging the CLI default' }).click();
  await expect(page.getByRole('status')).toContainText('staging is the CLI default now');
  await expect(
    page.getByRole('article', { name: 'staging' }).getByText('CLI default'),
  ).toBeVisible();
  await expect(page.getByRole('article', { name: 'prod-eu' }).getByText('CLI default')).toHaveCount(
    0,
  );

  await page.getByRole('button', { name: 'Open prod-eu' }).click();
  await page.getByRole('button', { name: 'Target', exact: true }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Target' })).toBeVisible();
  const card = page.getByRole('region', { name: 'Target' });
  await expect(card.getByRole('group', { name: 'Machine' })).toContainText(
    'apprafter backup create',
  );
  await expect(page.locator('.cluster-meta')).toHaveText('hetzner-cloud · nbg1 · T2');
  await expect(card.getByRole('group', { name: 'SSH key' })).toBeVisible();
  // Every row of the Target card on one line, its label whole and the long values cut short
  // instead: at the window's default size and at its smallest (src-tauri/src/window.rs).
  expect(await crampedRows(card)).toEqual([]);
  await shot(page, info, 'target screen');
  const size = page.viewportSize();
  await page.setViewportSize({ width: 1024, height: 640 });
  expect(await crampedRows(card)).toEqual([]);
  await card.getByRole('group', { name: 'Credentials file' }).scrollIntoViewIfNeeded();
  await shot(page, info, 'target screen, smallest window');
  if (size !== null) await page.setViewportSize(size);

  await page.getByRole('button', { name: 'Rename' }).click();
  const form = page.getByRole('dialog', { name: 'Rename target' });
  await form.getByLabel('New name').fill('prod-de');
  await form.getByRole('button', { name: 'Continue' }).click();
  const rename = page.getByRole('dialog', { name: 'Rename prod-eu to prod-de?' });
  await shot(page, info, 'rename confirm');
  await rename.getByRole('button', { name: 'Rename' }).click();
  await expect(page.getByRole('tab', { name: 'prod-de' })).toBeVisible();
  await expect(page.getByRole('status')).toContainText('Renamed prod-eu to prod-de');
  await expect(card.getByRole('group', { name: 'Config file' })).toContainText('/prod-de/');
  await expect(page.locator('.cluster-meta')).toHaveText('hetzner-cloud · nbg1 · T2');

  await page.getByRole('button', { name: 'Renew' }).click();
  const renew = page.getByRole('dialog', { name: 'Renew API token' });
  // Token-shaped, nobody's.
  await renew.getByRole('textbox', { name: 'Hetzner Cloud token' }).fill('k'.repeat(64));
  await renew.getByRole('button', { name: 'Continue' }).click();
  await page
    .getByRole('dialog', { name: 'Renew the API token of prod-de?' })
    .getByRole('button', { name: 'Renew' })
    .click();
  await expect(page.getByRole('status')).toContainText('Token renewed');

  // A key-only change: no token asked for, the credentials kept.
  await card
    .getByRole('group', { name: 'SSH key' })
    .getByRole('button', { name: 'Change' })
    .click();
  const key = page.getByRole('dialog', { name: 'Change SSH key' });
  await expect(key.getByRole('radio', { name: /~\/\.ssh\/id_ed25519\.pub/ })).toBeDisabled();
  await expect(key.getByRole('radio', { name: /~\/\.ssh\/work\.pub/ })).toBeChecked();
  await expect(key.getByRole('textbox', { name: 'Hetzner Cloud token' })).toHaveCount(0);
  await shot(page, info, 'change ssh key');
  await key.getByRole('button', { name: 'Continue' }).click();
  const keyConfirm = page.getByRole('dialog', { name: 'Change the SSH key of prod-de?' });
  await expect(keyConfirm).toContainText('~/.ssh/work.pub');
  await keyConfirm.getByRole('button', { name: 'Change key' }).click();
  await expect(page.getByRole('status')).toContainText('SSH key changed · credentials unchanged');
  await expect(card.getByRole('group', { name: 'SSH key' })).toContainText('~/.ssh/work.pub');

  await page.getByRole('button', { name: 'Remove…' }).click();
  const remove = page.getByRole('dialog', { name: 'Remove target prod-de?' });
  await remove.getByLabel(/Type prod-de to confirm/).fill('prod-de');
  await shot(page, info, 'remove confirm');
  await remove.getByRole('button', { name: 'Remove target' }).click();
  await expect(page.getByRole('tab', { name: 'prod-de' })).toHaveCount(0);
  await expect(page.getByRole('status')).toContainText('Removed prod-de from this computer');
});
