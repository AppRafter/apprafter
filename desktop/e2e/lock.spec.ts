// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The target store's lock on the mock IPC (D.4a §3.14; WI-476): the Security page — the store and
// every target it covers, reached from the Targets page's line, the sidebar and Settings — Protect
// from it end to end, a protected store's lock screen (its own field on every OS), the pinned
// settings in Settings › Lock (Auto-lock without Never, O7) and Stop protecting with the store's
// secret (O11). The mock's secrets are demo values: the lock password below and T24's first
// recovery-key test vector, nobody's.
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

/** The Security page from the sidebar's footer item; its view's main region. */
async function openSecurity(page: Page) {
  await page.getByRole('button', { name: 'Security', exact: true }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Security' })).toBeVisible();
  return page.getByRole('main');
}

test('Protect from the Security page: the password, the key shown, copied and typed back, the run', async ({
  page,
}, info) => {
  await page.goto(`/${queryOf(info)}`);
  await page.getByRole('button', { name: 'Unlock' }).click();
  const security = await openSecurity(page);
  await expect(security.getByText(/^Secrets are in files/)).toBeVisible();
  await security.getByRole('button', { name: 'Protect…' }).click();
  const wizard = page.getByRole('dialog', { name: 'Protect secrets' });
  await expect(wizard).toBeVisible();
  // The title on one line, clear of the five steps beside it: nothing overlaps.
  const fits = await wizard.getByRole('heading', { name: 'Protect secrets' }).evaluate((el) => {
    const lines =
      el.getBoundingClientRect().height / Number.parseFloat(getComputedStyle(el).lineHeight);
    return { overflows: el.scrollWidth > el.clientWidth, lines: Math.round(lines) };
  });
  expect(fits).toEqual({ overflows: false, lines: 1 });
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
  await expect(security.getByRole('button', { name: 'Stop protecting…' })).toBeVisible();
  await shot(page, info, 'protected');
});

test('a protected store: its own field on the lock screen, the pins in Settings, Stop protecting on the Security page', async ({
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
  await expect(
    page.getByText('Target store ~/.config/apprafter · Secrets: protected'),
  ).toBeVisible();

  const settings = await openSettings(page);
  await expect(settings.getByRole('switch', { name: 'Require unlock' })).toBeDisabled();
  await expect(settings.getByRole('switch', { name: 'Require unlock' })).toHaveAttribute(
    'aria-checked',
    'true',
  );
  const never = settings.getByRole('radio', { name: 'Never' });
  await expect(never).toHaveAttribute('aria-disabled', 'true');
  await expect(never).toHaveAttribute('title', 'Not while secrets are protected.');
  // And it looks unavailable, as a disabled option does: offered, it would read as a choice.
  await expect(never).toHaveCSS('opacity', '0.45');
  await expect(never).toHaveCSS('cursor', 'not-allowed');
  // Nothing of the store's own protection is in Settings any more (WI-476).
  await expect(settings.getByRole('button', { name: 'Stop protecting…' })).toHaveCount(0);
  await shot(page, info, 'settings, lock pinned');

  await settings.getByRole('button', { name: 'Open Security' }).click();
  await expect(settings).toBeHidden();
  await expect(page.getByRole('heading', { level: 1, name: 'Security' })).toBeVisible();
  const security = page.getByRole('main');
  await security.getByRole('button', { name: 'Stop protecting…' }).click();
  const confirm = page.getByRole('dialog', { name: 'Stop protecting secrets' });
  await confirm.getByLabel('Lock password or recovery key', { exact: true }).fill(STORE_PASSWORD);
  await shot(page, info, 'stop protecting, the store secret');
  await confirm.getByRole('button', { name: 'Stop protecting' }).click();
  await expect(page.getByText('Secrets are in files again.')).toBeVisible();
  await expect(security.getByRole('button', { name: 'Protect…' })).toBeVisible();
});

test('the Security page: the store and every target it covers, three ways in', async ({
  page,
}, info) => {
  const query = queryOf(info);
  // The mock is a Mac unless told otherwise: Mod is Cmd there, Ctrl on Windows.
  const mod = query.includes('os=windows') ? 'Control' : 'Meta';
  await page.goto(`/${query}`);
  await page.getByRole('button', { name: 'Unlock' }).click();

  // 1. From the Targets page's line.
  await expect(
    page.getByText('Target store ~/.config/apprafter · Secrets: in files'),
  ).toBeVisible();
  await shot(page, info, 'targets, the store line');
  await page.getByRole('button', { name: 'Security page' }).click();
  const title = page.getByRole('heading', { level: 1, name: 'Security' });
  await expect(title).toBeFocused();
  const security = page.getByRole('main');
  await expect(
    security.getByText('Target store ~/.config/apprafter', { exact: true }),
  ).toBeVisible();
  await expect(security.getByText('Covers 4 targets: broken, lab, prod-eu, staging')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Security', exact: true })).toHaveAttribute(
    'aria-current',
    'page',
  );
  await shot(page, info, 'security, a store in files');

  // 2. From a tab's sidebar: Enter on the focused item. That Tab reaches it is the bun Shell
  // test's ("reached by the keyboard") and the walk's (D-16).
  await page.keyboard.press(`${mod}+KeyT`);
  await page.getByRole('button', { name: 'Open prod-eu' }).click();
  await expect(page.getByRole('tab', { name: 'prod-eu' })).toHaveAttribute('aria-selected', 'true');
  await page.getByRole('button', { name: 'Security', exact: true }).focus();
  await page.keyboard.press('Enter');
  await expect(title).toBeFocused();

  // 3. From Settings, opened on the Security view itself: Open Security closes it, and the
  // focus goes on to the title, past the Settings button its close hands it back to (F4).
  const settings = await openSettings(page);
  await settings.getByRole('button', { name: 'Open Security' }).click();
  await expect(settings).toBeHidden();
  await expect(title).toBeFocused();

  // And Mod+L locks from the page, as from anywhere.
  await page.keyboard.press(`${mod}+KeyL`);
  await expect(page.getByRole('heading', { name: 'AppRafter is locked' })).toBeVisible();
});
