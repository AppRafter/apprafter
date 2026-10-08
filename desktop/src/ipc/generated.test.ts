// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A generated IPC type must not share its name with a global of the libs the app compiles
// against (tsconfig `lib`: the DOM and ES ones). A file that forgets the import would get the
// DOM's `Event`, `Notification` or `Lock` instead — and still type-check. Rust names the types,
// so a clash is fixed there (rename, or `#[ts(rename)]`), never in the generated file.
import { expect, test } from 'bun:test';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { exportedNames, globalNames, libReferences } from '../test/ts-names';

const desktop = join(import.meta.dir, '../..');
const generated = join(import.meta.dir, 'generated');

/**
 * TypeScript's lib directory. Up to TS 6 it is `typescript/lib`; TS 7 ships the libs beside its
 * native compiler, in the platform package (`@typescript/typescript-<os>-<arch>/lib`).
 */
function libDir(): string {
  const typescript = dirname(Bun.resolveSync('typescript/package.json', desktop));
  const classic = join(typescript, 'lib');
  if (existsSync(join(classic, 'lib.dom.d.ts'))) return classic;
  const platform = `@typescript/typescript-${process.platform}-${process.arch}/package.json`;
  const native = join(dirname(Bun.resolveSync(platform, typescript)), 'lib');
  if (existsSync(join(native, 'lib.dom.d.ts'))) return native;
  throw new Error(`no lib.dom.d.ts in ${classic} or ${native}`);
}

/** Every global the tsconfig `lib` entries declare, following their `<reference lib>`s. */
async function libGlobals(): Promise<Set<string>> {
  const tsconfig = await Bun.file(join(desktop, 'tsconfig.json')).json();
  const dir = libDir();
  const pending: string[] = tsconfig.compilerOptions.lib.map((lib: string) => lib.toLowerCase());
  const read = new Set<string>();
  const names = new Set<string>();
  for (let lib = pending.pop(); lib !== undefined; lib = pending.pop()) {
    if (read.has(lib)) continue;
    read.add(lib);
    const text = await Bun.file(join(dir, `lib.${lib}.d.ts`)).text();
    for (const name of globalNames(text)) names.add(name);
    pending.push(...libReferences(text));
  }
  return names;
}

test('no generated IPC type or constant shares its name with a global', async () => {
  const globals = await libGlobals();
  // The scan reached both the DOM and the ES libs, so an empty scan cannot pass.
  for (const name of ['Event', 'Notification', 'Lock', 'Range', 'Report', 'Error', 'Record']) {
    expect(globals.has(name), name).toBe(true);
  }
  const exported: { file: string; name: string }[] = [];
  for await (const file of new Bun.Glob('**/*.ts').scan({ cwd: generated })) {
    const source = await Bun.file(join(generated, file)).text();
    for (const name of exportedNames(source)) exported.push({ file, name });
  }
  const names = exported.map((e) => e.name);
  for (const name of ['Settings', 'UiError', 'JsonValue', 'COMMANDS', 'LOCK_CHANGED']) {
    expect(names, name).toContain(name);
  }
  const clashes = exported.filter((e) => globals.has(e.name)).map((e) => `${e.file}: ${e.name}`);
  expect(clashes).toEqual([]);
});
