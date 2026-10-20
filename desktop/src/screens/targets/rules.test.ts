// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { HETZNER_TOKEN_LEN, TARGET_NAME_MAX_LEN } from '../../ipc/generated/target';
import { type NameProblem, nameMessage, nameProblem, tokenMessage, tokenProblem } from './rules';

const cases = (await Bun.file(
  new URL('../../ipc/generated/fixtures/target-names.json', import.meta.url),
).json()) as { name: string; problem: NameProblem | null }[];

test('the name rule answers as the core does, case by case', () => {
  expect(cases.length).toBeGreaterThan(12);
  for (const c of cases) expect({ name: c.name, problem: nameProblem(c.name) }).toEqual(c);
});

test('every problem has a message, and a good name none', () => {
  for (const p of ['empty', 'too_long', 'invalid_char', 'edge_dash'] as const) {
    expect(nameMessage(p)?.length ?? 0).toBeGreaterThan(0);
  }
  expect(nameMessage(null)).toBeNull();
  expect(nameMessage('too_long')).toContain(String(TARGET_NAME_MAX_LEN));
});

test('a token is HETZNER_TOKEN_LEN letters and digits, the length checked first', () => {
  const good = 'k'.repeat(HETZNER_TOKEN_LEN);
  expect(tokenProblem(good)).toBeNull();
  expect(tokenMessage(good)).toBeNull();
  expect(tokenProblem(good.slice(1))).toBe('wrong_length');
  expect(tokenProblem(`${good.slice(1)}-`)).toBe('not_alphanumeric');
  expect(tokenProblem(`ä${good.slice(2)}`)).toBe('not_alphanumeric'); // ä is 2 bytes: the length fits
  expect(tokenProblem(`ä${good.slice(1)}`)).toBe('wrong_length'); // 65 bytes: the length first
  expect(tokenMessage(good.slice(1))).toContain(String(HETZNER_TOKEN_LEN));
  expect(tokenMessage(good.slice(1))).toContain(String(HETZNER_TOKEN_LEN - 1));
  expect(tokenMessage(`${good.slice(1)}-`)?.length ?? 0).toBeGreaterThan(0);
});
