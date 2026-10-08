// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The desktop and the landing page share one palette, and the desktop's colours are the design
// file's THEMES. Both are compared by effective value per theme: landing's light block inherits
// the accent tokens from :root, so its declarations alone would not say what light renders.
import { describe, expect, test } from 'bun:test';
import { join } from 'node:path';
import { effectiveProperties, normalizeValue } from '../test/css-vars';

const read = (relative: string) => Bun.file(join(import.meta.dir, relative)).text();

// Brief §1.1: the 12 tokens THEMES shares with landing, plus landing's --code-border.
const SHARED = [
  '--bg',
  '--surface',
  '--surface-2',
  '--fg',
  '--fg-muted',
  '--fg-faint',
  '--accent',
  '--accent-2',
  '--accent-fg',
  '--border',
  '--border-strong',
  '--code-bg',
  '--code-border',
] as const;

// <html> without data-theme renders dark, as data-theme='dark' does.
type Resolved = 'dark' | 'light';
const STATES: { label: string; attribute: Resolved | undefined; theme: Resolved }[] = [
  { label: 'no data-theme', attribute: undefined, theme: 'dark' },
  { label: "data-theme='dark'", attribute: 'dark', theme: 'dark' },
  { label: "data-theme='light'", attribute: 'light', theme: 'light' },
];

const ours = await read('./tokens.css');
const landing = await read('../../../landing/web/src/styles/tokens.css');

/** The design file's `THEMES = { dark: {…}, light: {…} }`, read from its script. */
async function designThemes(): Promise<Record<Resolved, Record<string, string>>> {
  const html = await read('../../design-source/AppRafterApp.dc.html');
  const found = html.match(/THEMES = \{\s*dark: (\{[^}]*\}),\s*light: (\{[^}]*\})/);
  if (found?.[1] === undefined || found[2] === undefined)
    throw new Error('no THEMES in the design');
  const parse = (object: string) => JSON.parse(object.replaceAll("'", '"'));
  return { dark: parse(found[1]), light: parse(found[2]) };
}

describe.each(STATES)('$label', ({ attribute, theme }) => {
  test('the shared tokens equal landing', () => {
    const desktop = effectiveProperties(ours, attribute);
    const site = effectiveProperties(landing, attribute);
    for (const name of SHARED) {
      expect(site.get(name), `landing ${name}`).toBeDefined();
      expect(desktop.get(name), name).toBe(site.get(name));
    }
  });

  test(`every THEMES key has the design's ${theme} value`, async () => {
    const design = (await designThemes())[theme];
    expect(Object.keys(design)).toHaveLength(27);
    const desktop = effectiveProperties(ours, attribute);
    for (const [name, value] of Object.entries(design)) {
      expect(desktop.get(name), name).toBe(normalizeValue(value));
    }
  });
});
