// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { exportedNames, globalNames, libReferences } from './ts-names';

describe('exportedNames', () => {
  test('reads exported types, interfaces and consts, multi-line ones included', () => {
    const source = [
      '// SPDX-License-Identifier: FSL-1.1-Apache-2.0',
      'import type { OpId } from "./OpId";',
      '',
      '/** A doc comment that says export type Fake = 1. */',
      'export type OpEvent = { "kind": "stage", index: number, } | { "kind": "warning", };',
      'export interface Shape { a: number }',
      'export const COMMANDS = [',
      "  'activity',",
      '] as const;',
    ].join('\n');
    expect(exportedNames(source)).toEqual(['OpEvent', 'Shape', 'COMMANDS']);
  });

  test('refuses an export form it does not read, rather than skip it', () => {
    expect(() => exportedNames('export enum Kind { A }')).toThrow('export enum Kind');
    expect(() => exportedNames('export { Kind } from "./Kind";')).toThrow('export {');
    expect(() => exportedNames('export default 1;')).toThrow('export default');
  });
});

describe('globalNames', () => {
  test('reads the top-level declarations of a lib file, not namespace members', () => {
    const lib = [
      '/// <reference no-default-lib="true"/>',
      'interface Event {',
      '    readonly type: string;',
      '}',
      'declare var Event: {',
      '    prototype: Event;',
      '};',
      'type HeadersInit = [string, string][];',
      'declare function fetch(input: string): Promise<Response>;',
      'declare namespace WebAssembly {',
      '    interface Global { value: any }',
      '    var Global: { prototype: Global };',
      '}',
      'declare class Lock {}',
    ].join('\n');
    expect(globalNames(lib).sort()).toEqual(
      ['Event', 'HeadersInit', 'Lock', 'WebAssembly', 'fetch'].sort(),
    );
  });
});

test('libReferences reads the libs a lib file pulls in', () => {
  const lib = [
    '/// <reference no-default-lib="true"/>',
    '/// <reference lib="es2015" />',
    '/// <reference lib="es2018.asynciterable" />',
  ].join('\n');
  expect(libReferences(lib)).toEqual(['es2015', 'es2018.asynciterable']);
});
