// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { exportedNames } from './ts-names';

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
