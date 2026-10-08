// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Names a TypeScript file exports, and names a lib file declares globally — read line by line
// from column 0, which is how ts-rs writes its exports and how TypeScript's lib files write
// their globals (namespace members are indented).

const IDENT = '[A-Za-z_$][\\w$]*';
const EXPORT = new RegExp(`^export (?:type|interface|const) (${IDENT})`);
const GLOBAL = new RegExp(
  `^(?:interface|type|declare (?:var|let|const|function|class|namespace|enum)) (${IDENT})`,
  'gm',
);

/** Every `export type|interface|const X` at the start of a line; any other export throws. */
export function exportedNames(source: string): string[] {
  const names: string[] = [];
  for (const line of source.split('\n')) {
    if (!/^export\b/.test(line)) continue;
    const name = line.match(EXPORT)?.[1];
    if (name === undefined) throw new Error(`an export form this scanner does not read: ${line}`);
    names.push(name);
  }
  return names;
}

/** The top-level declarations of a lib `.d.ts`, once each: the names it puts in global scope. */
export function globalNames(lib: string): string[] {
  return [...new Set([...lib.matchAll(GLOBAL)].map((m) => m[1] ?? ''))];
}

/** The `/// <reference lib="…" />` a lib file pulls in. */
export function libReferences(lib: string): string[] {
  return [...lib.matchAll(/^\/\/\/ <reference lib="([\w.]+)" \/>/gm)].map((m) => m[1] ?? '');
}
