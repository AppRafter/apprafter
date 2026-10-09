// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The OS-authentication screens on the mock IPC: the lock screen's own password field on Linux's
// PAM route (`?auth=pam`), its back-off counted down on Playwright's clock, and the lock-on-sleep
// row where the computer tells the app nothing of its session (`?session=none`).
import { expect, type Page, type TestInfo, test } from '@playwright/test';

/** The mock's demo password (MOCK_PASSWORD in src/ipc/mock), nobody's real one. */
const MOCK_PASSWORD = 'apprafter';

async function shot(page: Page, info: TestInfo, name: string) {
  await info.attach(name, { body: await page.screenshot(), contentType: 'image/png' });
}

const queryOf = (info: TestInfo) => String(info.project.metadata.query ?? '');

test('the PAM route: a wrong password says what PAM said, the right one unlocks', async ({
  page,
}, info) => {
  const query = queryOf(info);
  test.skip(query.includes('os=windows'), 'the PAM route is Linux’s');
  await page.goto(`/${query}&os=linux&auth=pam`);
  await expect(page.getByRole('heading', { name: 'AppRafter is locked' })).toBeVisible();
  const field = page.getByLabel('System password', { exact: true });
  await expect(field).toBeFocused();
  await expect(page.getByRole('button', { name: 'Unlock' })).toBeDisabled();
  await shot(page, info, 'lock screen, password field');

  await field.fill('not the password');
  await field.press('Enter');
  await expect(page.getByRole('alert')).toHaveText('Authentication failure');
  await expect(field).toHaveValue('');
  await shot(page, info, 'lock screen, wrong password');

  await field.fill(MOCK_PASSWORD);
  await page.getByRole('button', { name: 'Unlock' }).click();
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
});

test('too many wrong passwords: the field counts down what Rust says, then unlocks', async ({
  page,
}, info) => {
  const query = queryOf(info);
  test.skip(query.includes('os=windows'), 'the PAM route is Linux’s');
  // The page's clock (setTimeout, performance.now) is Playwright's from the first load.
  await page.clock.install();
  await page.goto(`/${query}&os=linux&auth=pam`);
  const field = page.getByLabel('System password', { exact: true });
  await expect(field).toBeFocused();
  // From here time moves only when the test says.
  await page.clock.pauseAt(Date.now() + 60_000);

  const alert = page.getByRole('alert');
  for (const attempt of [1, 2]) {
    await field.fill(`not the password ${attempt}`);
    await field.press('Enter');
    await expect(alert).toHaveText('Authentication failure');
  }
  await field.fill('not the password 3');
  await field.press('Enter');
  await expect(alert).toHaveText(
    'Authentication failureToo many failed attempts. Try again in 30 s.',
  );
  await expect(field).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Unlock' })).toBeDisabled();
  await shot(page, info, 'lock screen, too many failed attempts');

  await page.clock.runFor(10_000);
  await expect(alert).toHaveText(
    'Authentication failureToo many failed attempts. Try again in 20 s.',
  );
  await expect(field).toBeDisabled();
  await page.clock.runFor(20_000);
  await expect(field).toBeEnabled();
  await expect(field).toBeFocused();
  await expect(alert).toHaveText('Authentication failure');

  await page.clock.resume();
  await field.fill(MOCK_PASSWORD);
  await field.press('Enter');
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
});

test('lock on sleep is disabled, saying why, where the computer tells nothing', async ({
  page,
}, info) => {
  await page.goto(`/${queryOf(info)}&session=none`);
  await page.getByRole('button', { name: 'Unlock' }).click();
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
  await page.getByRole('button', { name: /^Settings/ }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await expect(
    settings.getByRole('switch', { name: 'Lock when the computer sleeps or locks' }),
  ).toBeDisabled();
  await expect(
    settings.getByText('This computer does not tell AppRafter when it locks or sleeps.'),
  ).toBeVisible();
  await shot(page, info, 'settings, nothing told of the session');
});
