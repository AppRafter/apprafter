// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Layout the shell's behaviour depends on, read from the stylesheet (happy-dom lays nothing
// out): tabs that still fit, or scroll, past the window width; and each view a stacking context
// of its own, so a tab's dialog never paints over Settings.
import { expect, test } from 'bun:test';
import { join } from 'node:path';

const read = (name: string) => Bun.file(join(import.meta.dir, name)).text();

/** The body of the plain rule `selector { ... }`; one per selector in shell.css. */
async function rule(selector: string): Promise<string> {
  const css = (await read('shell.css')).replace(/\/\*[\s\S]*?\*\//g, '');
  const found = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].filter(
    ([, name = '']) => name.trim() === selector,
  );
  expect(found, selector).toHaveLength(1);
  return found[0]?.[2] ?? '';
}

test('a tab shrinks from 184px to 96px, then the strip scrolls: every tab stays reachable', async () => {
  const tab = await rule('.tab');
  expect(tab).toMatch(/flex:\s*0 1 184px;/);
  expect(tab).toMatch(/min-width:\s*96px;/);
  expect(tab).not.toMatch(/(^|[\s;])width:/);
  expect(await rule('.tabs')).toMatch(/overflow-x:\s*auto;/);
});

test('each view is a stacking context, so its dialogs stay under the app-wide Settings', async () => {
  expect(await rule('.view')).toMatch(/isolation:\s*isolate;/);
});
