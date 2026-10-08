// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// No webview storage: state lives in Rust, and WKWebView keeps its storage outside the data
// directory the app is pointed at, so a walk's isolation would leak through it. Biome's
// noRestrictedGlobals catches the bare globals; this catches every other spelling
// (`window.localStorage`, `globalThis['indexedDB']`), comments included.
import { expect, test } from 'bun:test';
import { join, relative } from 'node:path';

const NAMES = ['localStorage', 'sessionStorage', 'indexedDB'];
const desktop = join(import.meta.dir, '..');
const self = relative(desktop, import.meta.path);

test('no source file names webview storage', async () => {
  const pattern = new RegExp(`\\b(?:${NAMES.join('|')})\\b`);
  const found: string[] = [];
  for await (const path of new Bun.Glob('src/**/*.{ts,tsx}').scan({ cwd: desktop })) {
    if (path === self) continue;
    const lines = (await Bun.file(join(desktop, path)).text()).split('\n');
    lines.forEach((line, i) => {
      if (pattern.test(line)) found.push(`${path}:${i + 1}`);
    });
  }
  expect(found).toEqual([]);
});
