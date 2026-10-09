// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { HETZNER_TOKEN_LEN } from '../../ipc/generated/target';
import { tokenMessage } from '../targets/rules';
import { tokenHint } from './tokenHint';

test('empty: where the token comes from', () => {
  expect(tokenHint('')).toBe('Cloud Console → Security → API Tokens, read & write.');
});

test('while typing: the byte count, as the core counts', () => {
  expect(tokenHint('abc')).toBe(`3/${HETZNER_TOKEN_LEN} characters`);
  expect(tokenHint('é')).toBe(`2/${HETZNER_TOKEN_LEN} characters`);
});

test("a character the core refuses: D.3d's message, word for word", () => {
  const bad = `${'a'.repeat(HETZNER_TOKEN_LEN - 1)}-`;
  expect(tokenHint(bad)).toBe(tokenMessage(bad) ?? '');
});
