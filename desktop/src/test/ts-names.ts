// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The names a TypeScript file exports, read line by line from column 0, which is how ts-rs
// writes its exports.

const IDENT = '[A-Za-z_$][\\w$]*';
const EXPORT = new RegExp(`^export (?:type|interface|const) (${IDENT})`);

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
