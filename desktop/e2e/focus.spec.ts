// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The dialogs' Tab order where only a browser can show it, in every project of the config. A
// body that scrolls and holds no control is a Tab stop of Chromium's own (130 and later, and
// WebView2) and not of WebKit's; the trap makes it one on both, so Tab goes from Close to the
// body, the footer and round again — never from the body back to Close, the footer unreached.
// The case: the toolchain with every tool found (`?tools=found`), at the smallest window.
import { expect, type Page, test } from '@playwright/test';

/** What has the focus: the dialog's body, a footer's summary, or a button by its name. */
function focused(page: Page): Promise<string> {
  return page.evaluate(() => {
    const active = document.activeElement;
    if (active === null) return 'nothing';
    if (active.matches('[data-modal-body]')) return 'body';
    if (active.tagName === 'SUMMARY') return 'summary';
    return active.getAttribute('aria-label') ?? active.textContent?.trim() ?? active.tagName;
  });
}

async function press(page: Page, key: string, times: number): Promise<string[]> {
  const seen: string[] = [];
  for (let i = 0; i < times; i += 1) {
    await page.keyboard.press(key);
    seen.push(await focused(page));
  }
  return seen;
}

test('the toolchain with every tool found: Tab reaches its body and its footer', async ({
  page,
}, info) => {
  // The window's smallest size (src-tauri/src/window.rs): six rows overflow the body here.
  await page.setViewportSize({ width: 1024, height: 640 });
  const query = String(info.project.metadata.query ?? '');
  await page.goto(`/${query}${query === '' ? '?' : '&'}tools=found`);
  await page.getByRole('button', { name: 'Unlock' }).click();
  await expect(page.getByRole('heading', { name: 'Open a cluster' })).toBeVisible();
  await page.getByRole('button', { name: /^Settings/ }).click();
  await page
    .getByRole('dialog', { name: 'Settings' })
    .getByRole('button', { name: 'Show', exact: true })
    .click();
  const toolchain = page.getByRole('dialog', { name: 'Toolchain' });
  await expect(toolchain.getByText('cue version v0.17.1')).toBeVisible();
  const body = toolchain.locator('[data-modal-body]');
  expect(await body.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(true);
  await expect(body.getByRole('button')).toHaveCount(0);

  await toolchain.getByRole('button', { name: 'Close' }).focus();
  expect(await press(page, 'Tab', 4)).toEqual(['body', 'summary', 'Check again', 'Close']);
  expect(await press(page, 'Shift+Tab', 4)).toEqual(['Check again', 'summary', 'body', 'Close']);

  // The body the keyboard reached scrolls by the keyboard.
  await page.keyboard.press('Tab');
  expect(await focused(page)).toBe('body');
  await page.keyboard.press('End');
  await expect.poll(() => body.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
});
