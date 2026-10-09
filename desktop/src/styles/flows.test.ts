// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Layout the D.3 flows depend on, read from the stylesheets (happy-dom lays nothing out).
import { expect, test } from 'bun:test';
import { join } from 'node:path';

const read = (name: string) => Bun.file(join(import.meta.dir, name)).text();
async function rule(file: string, selector: string): Promise<string> {
  const css = (await read(file)).replace(/\/\*[\s\S]*?\*\//g, '');
  const found = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].filter(
    ([, name = '']) => name.trim() === selector,
  );
  expect(found, selector).toHaveLength(1);
  return found[0]?.[2] ?? '';
}

test('the table scrolls in its box and its header stays in view', async () => {
  expect(await rule('components.css', '.data-table-scroll')).toMatch(/overflow:\s*auto;/);
  const th = await rule('components.css', '.data-table thead th');
  expect(th).toMatch(/position:\s*sticky;/);
  expect(th).toMatch(/top:\s*0;/);
});

test('the sticky header keeps its bottom border: borders are separate, not collapsed', async () => {
  // A collapsed border belongs to the table, not to the cell: it stays behind while the header
  // sticks, and the rows scroll under a header with no line below it.
  const table = await rule('components.css', '.data-table');
  expect(table).toMatch(/border-collapse:\s*separate;/);
  expect(table).toMatch(/border-spacing:\s*0;/);
  expect(await rule('components.css', '.data-table thead th')).toMatch(/border-bottom:/);
});

test('a row the keyboard moves to scrolls into view below the header, not under it', async () => {
  expect(await rule('components.css', '.data-table-scroll')).toMatch(
    /scroll-padding-top:\s*var\(--h-30\);/,
  );
  expect(await rule('components.css', '.data-table thead th')).toMatch(/height:\s*var\(--h-30\);/);
});
