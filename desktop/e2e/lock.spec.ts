// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The target store's lock on the mock IPC (D.4a §3.14): Protect from Settings end to end, a
// protected store's lock screen (its own field on every OS), the pinned settings (Auto-lock
// without Never, O7) and Stop protecting with the store's secret (O11). The mock's secrets are
// demo values: the lock password below and T24's first recovery-key test vector, nobody's.
import { expect, type Page, type TestInfo, test } from '@playwright/test';

const STORE_PASSWORD = 'correct horse battery staple';
const RECOVERY_KEY =
  'APPRAFTER-RECOVERY-KEY-1\nQQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK';

async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(name, { body: await page.screenshot(), contentType: 'image/png' });
}

const queryOf = (info: TestInfo) => String(info.project.metadata.query ?? '?');

async function openSettings(page: Page) {
  await page.getByRole('button', { name: /^Settings/ }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(settings).toBeVisible();
  return settings;
}

test('Protect: the password, the key shown, copied and typed back, the run', async ({
  page,
}, info) => {
  await page.goto(`/${queryOf(info)}`);
  await page.getByRole('button', { name: 'Unlock' }).click();
  const settings = await openSettings(page);
  await expect(settings.getByText(/^Secrets are in files/)).toBeVisible();
  await settings.getByRole('button', { name: 'Protect…' }).click();
  const wizard = page.getByRole('dialog', { name: 'Protect secrets' });
  await expect(wizard).toBeVisible();
  await shot(page, info, 'protect, what it does');
  await wizard.getByRole('button', { name: 'Continue' }).click();

  await wizard.getByLabel('Lock password', { exact: true }).fill('short');
  await expect(wizard.getByText(/^Shorter than 15 characters/)).toBeVisible();
  await wizard.getByLabel('Lock password', { exact: true }).fill(STORE_PASSWORD);
  await wizard.getByLabel('Repeat the lock password', { exact: true }).fill(STORE_PASSWORD);
  await shot(page, info, 'protect, the password');
  await wizard.getByRole('button', { name: 'Continue' }).click();

  const key = wizard.getByLabel('Recovery key', { exact: true });
  await expect(key).toContainText('QQQSY-QCYQ5');
  // Only Rust's Copy copies it: the browser's own selection is off (ADR 0068 §3, O12).
  const selectable = await key.evaluate((el) => {
    const style = getComputedStyle(el) as CSSStyleDeclaration & { webkitUserSelect?: string };
    return style.userSelect || style.webkitUserSelect || '';
  });
  expect(selectable).toBe('none');
  await wizard.getByRole('button', { name: 'Copy' }).click();
  await expect(page.getByText('Copied. The clipboard is cleared in 60 s.')).toBeVisible();
  await shot(page, info, 'protect, the recovery key');
  await wizard.getByRole('button', { name: 'I saved it' }).click();

  // exact: the reveal button's "Show …" label holds the same words (UI verifier M2).
  const typed = wizard.getByLabel('Type the whole recovery key to confirm you saved it', {
    exact: true,
  });
  await expect(typed).toHaveAttribute('type', 'password');
  await typed.fill('APPRAFTER-RECOVERY-KEY-1 QQQQQ');
  await wizard.getByRole('button', { name: 'Protect' }).click();
  await expect(wizard.getByRole('alert')).toBeVisible();
  await typed.fill(RECOVERY_KEY.replace('\n', ' '));
  await wizard.getByRole('button', { name: 'Protect' }).click();
  await expect(
    page.getByText('Secrets protected. Unlocking AppRafter now opens them.'),
  ).toBeVisible();
  await expect(settings.getByRole('button', { name: 'Stop protecting…' })).toBeVisible();
  await shot(page, info, 'protected');
});

test('a protected store: its own field on the lock screen, the pins, Stop protecting', async ({
  page,
}, info) => {
  await page.goto(`/${queryOf(info)}&secrets=locked`);
  await expect(page.getByRole('heading', { name: 'AppRafter is locked' })).toBeVisible();
  const field = page.getByPlaceholder('Lock password or recovery key');
  await expect(field).toBeVisible();
  await expect(
    page.getByText('Clusters keep running. Credentials stay encrypted until you unlock.'),
  ).toBeVisible();
  await field.fill('not it');
  await field.press('Enter');
  await expect(page.getByRole('alert')).toHaveText(
    'That is not the lock password or the recovery key.',
  );
  await shot(page, info, 'protected lock screen, wrong secret');
  await field.fill(STORE_PASSWORD);
  await field.press('Enter');
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();

  const settings = await openSettings(page);
  await expect(settings.getByRole('switch', { name: 'Require unlock' })).toBeDisabled();
  await expect(settings.getByRole('switch', { name: 'Require unlock' })).toHaveAttribute(
    'aria-checked',
    'true',
  );
  const never = settings.getByRole('radio', { name: 'Never' });
  await expect(never).toHaveAttribute('aria-disabled', 'true');
  await expect(never).toHaveAttribute('title', 'Not while secrets are protected.');
  await shot(page, info, 'settings, lock pinned');

  await settings.getByRole('button', { name: 'Stop protecting…' }).click();
  const confirm = page.getByRole('dialog', { name: 'Stop protecting secrets' });
  await confirm.getByLabel('Lock password or recovery key', { exact: true }).fill(STORE_PASSWORD);
  await shot(page, info, 'stop protecting, the store secret');
  await confirm.getByRole('button', { name: 'Stop protecting' }).click();
  await expect(page.getByText('Secrets are in files again.')).toBeVisible();
  await expect(settings.getByRole('button', { name: 'Protect…' })).toBeVisible();
});
