// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A generated IPC type must not share its name with a global the app's code can see: the DOM
// and ES libs (tsconfig `lib`) and the ambient types (`types`: bun-types, and @types/node through
// it). A file that forgets the import would get the DOM's `Event`, Node's `Buffer` or Bun's `Bun`
// instead — and still type-check. Rust names the types, so a clash is fixed there (rename, or
// `#[ts(rename)]`), never in the generated file.
//
// The compiler answers which names are global: a probe script refers to each name as a type and
// as a value, under the desktop's own `lib` and `types`; every name that is not "Cannot find
// name" exists in the global scope.
import { expect, test } from 'bun:test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { exportedNames } from '../test/ts-names';

const desktop = join(import.meta.dir, '../..');
const generated = join(import.meta.dir, 'generated');

// Present in each layer, so a probe that cannot see a layer fails here.
const KNOWN_GLOBALS = [
  'Event',
  'Notification',
  'Lock',
  'Error',
  'Record',
  'Bun',
  'Buffer',
  'process',
];

/** Which of `names` the compiler finds in the global scope of the app's own configuration. */
async function globalsAmong(names: readonly string[]): Promise<Set<string>> {
  const tsconfig = await Bun.file(join(desktop, 'tsconfig.json')).json();
  const dir = await mkdtemp(join(tmpdir(), 'apprafter-globals-'));
  try {
    const probe = names
      .flatMap((name) => [
        `type __type_${name} = ${name};`,
        `type __value_${name} = typeof ${name};`,
      ])
      .join('\n');
    await Bun.write(join(dir, 'probe.ts'), `${probe}\n`);
    await Bun.write(
      join(dir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          lib: tsconfig.compilerOptions.lib,
          types: tsconfig.compilerOptions.types,
          typeRoots: [join(desktop, 'node_modules/@types')],
          noEmit: true,
          skipLibCheck: true,
          strict: true,
        },
        files: ['probe.ts'],
      }),
    );
    const typescript = dirname(Bun.resolveSync('typescript/package.json', desktop));
    // Run from the probe's directory: tsc prints paths relative to where it runs.
    const run = Bun.spawnSync(
      [process.execPath, join(typescript, 'bin/tsc'), '-p', 'tsconfig.json', '--pretty', 'false'],
      { cwd: dir },
    );
    const output = run.stdout.toString();
    const errors = [...output.matchAll(/^probe\.ts\((\d+),\d+\): error TS\d+: (.*)$/gm)];
    if (errors.length === 0 && run.exitCode !== 0) {
      throw new Error(`tsc failed:\n${output}${run.stderr.toString()}`);
    }
    // Line 2i+1 names names[i] as a type, line 2i+2 as a value.
    const missing = new Map<string, number>();
    for (const [, line, message] of errors) {
      if (!message?.startsWith('Cannot find name')) continue;
      const name = names[Math.floor((Number(line) - 1) / 2)];
      if (name !== undefined) missing.set(name, (missing.get(name) ?? 0) + 1);
    }
    return new Set(names.filter((name) => missing.get(name) !== 2));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

test('no generated IPC type or constant shares its name with a global', async () => {
  const exported: { file: string; name: string }[] = [];
  for await (const file of new Bun.Glob('**/*.ts').scan({ cwd: generated })) {
    const source = await Bun.file(join(generated, file)).text();
    for (const name of exportedNames(source)) exported.push({ file, name });
  }
  const names = exported.map((e) => e.name);
  for (const name of ['Settings', 'UiError', 'JsonValue', 'COMMANDS', 'LOCK_CHANGED']) {
    expect(names, name).toContain(name);
  }
  const globals = await globalsAmong([...new Set([...KNOWN_GLOBALS, ...names])]);
  for (const name of KNOWN_GLOBALS) expect(globals.has(name), name).toBe(true);
  const clashes = exported.filter((e) => globals.has(e.name)).map((e) => `${e.file}: ${e.name}`);
  expect(clashes).toEqual([]);
});
