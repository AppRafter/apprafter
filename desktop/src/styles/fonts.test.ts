// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The self-hosted faces are the @fontsource packages' own files, ranges and licences: a package
// bump that changes a file fails here until the copy is redone.
import { expect, test } from 'bun:test';
import { readdir } from 'node:fs/promises';
import { join } from 'node:path';

const desktop = join(import.meta.dir, '../..');
const assets = join(desktop, 'src/assets/fonts');
const packageDir = (family: string) => join(desktop, 'node_modules/@fontsource', family);

const FACES = [
  { family: 'Roboto', pkg: 'roboto', weights: ['400', '500', '700'] },
  { family: 'Roboto Mono', pkg: 'roboto-mono', weights: ['400', '500'] },
] as const;
// In the package's order: where ranges overlap (U+0304 is in latin and latin-ext), the face
// declared last is tried first.
const SUBSETS = ['cyrillic', 'latin-ext', 'latin'] as const;

interface Face {
  family: string;
  weight: string;
  file: string;
  range: string;
}

/** A `unicode-range` list without whitespace, however the formatter wrapped it. */
const compact = (range: string) => range.replace(/\s+/g, '');

function declared(css: string): Face[] {
  return [...css.matchAll(/@font-face\s*\{([^}]*)\}/g)].map(([, body = '']) => {
    const field = (name: string) => body.match(new RegExp(`${name}:\\s*([^;]+);`))?.[1]?.trim();
    return {
      family: field('font-family')?.replace(/['"]/g, '') ?? '',
      weight: field('font-weight') ?? '',
      file: field('src')?.match(/url\(['"]?\.\.\/assets\/fonts\/([\w.-]+)['"]?\)/)?.[1] ?? '',
      range: compact(field('unicode-range') ?? ''),
    };
  });
}

/** The package's own `unicode-range` for one of its woff2 files. */
async function packageRange(pkg: string, weight: string, file: string): Promise<string> {
  const css = await Bun.file(join(packageDir(pkg), `${weight}.css`)).text();
  const escaped = file.replaceAll('.', '\\.');
  const range = css.match(
    new RegExp(`url\\(\\./files/${escaped}\\)[^;]*;\\s*unicode-range:([^;]+);`),
  );
  if (range?.[1] === undefined) throw new Error(`${pkg}/${weight}.css has no ${file}`);
  return compact(range[1]);
}

const fontsCss = await Bun.file(join(import.meta.dir, 'fonts.css')).text();

test('fonts.css declares each weight and subset once, with the package range and file', async () => {
  const faces = declared(fontsCss);
  const expected: Face[] = [];
  for (const { family, pkg, weights } of FACES) {
    for (const weight of weights) {
      for (const subset of SUBSETS) {
        const file = `${pkg}-${subset}-${weight}-normal.woff2`;
        expected.push({ family, weight, file, range: await packageRange(pkg, weight, file) });
      }
    }
  }
  expect(faces).toEqual(expected);
});

test('every shipped file is the package file, and the licences ship beside them', async () => {
  // shipped name -> the package file it copies
  const sources = new Map<string, string>();
  for (const { pkg, weights } of FACES) {
    const manifest = await Bun.file(join(packageDir(pkg), 'package.json')).json();
    expect(manifest.license, pkg).toBe('OFL-1.1');
    sources.set(`LICENSE-${pkg}.txt`, join(packageDir(pkg), 'LICENSE'));
    for (const weight of weights) {
      for (const subset of SUBSETS) {
        const file = `${pkg}-${subset}-${weight}-normal.woff2`;
        sources.set(file, join(packageDir(pkg), 'files', file));
      }
    }
  }
  expect((await readdir(assets)).sort()).toEqual([...sources.keys()].sort());
  for (const [name, source] of sources) {
    const ours = new Uint8Array(await Bun.file(join(assets, name)).arrayBuffer());
    const theirs = new Uint8Array(await Bun.file(source).arrayBuffer());
    expect(ours.length, name).toBeGreaterThan(0);
    expect(Bun.deepEquals(ours, theirs), name).toBe(true);
  }
});
