// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The webview side of overview §6.3: the mock IPC holds nothing token-shaped (its fixtures end
// up in screenshots and docs), and no generated IPC type has a string field that could carry a
// token, a password or a secret to the page.
import { expect, test } from 'bun:test';
import { join } from 'node:path';

const TOKEN_SHAPED = /[A-Za-z0-9]{64}/;
const SECRET_FIELD = /\b(token|password|secret)\??\s*:\s*string\b/i;

async function hits(dir: string, pattern: string, rule: RegExp): Promise<string[]> {
  const found: string[] = [];
  for await (const file of new Bun.Glob(pattern).scan({ cwd: dir })) {
    const match = (await Bun.file(join(dir, file)).text()).match(rule);
    if (match !== null) found.push(`${file}: ${match[0]}`);
  }
  return found;
}

test('the rules see what they look for', () => {
  expect('a'.repeat(64)).toMatch(TOKEN_SHAPED);
  expect('a'.repeat(63)).not.toMatch(TOKEN_SHAPED);
  expect('  token: string;').toMatch(SECRET_FIELD);
  expect('  password?: string;').toMatch(SECRET_FIELD);
  expect('  token: TokenPresence;').not.toMatch(SECRET_FIELD);
});

test('the scan reads the files it names', async () => {
  const mock = join(import.meta.dir, 'mock');
  expect(await hits(mock, '**/*', /MOCK_REPORTS/)).toContain('fixtures.ts: MOCK_REPORTS');
  const generated = join(import.meta.dir, 'generated');
  expect(await hits(generated, '**/*.ts', /export type TokenPresence/)).toContain(
    'TokenPresence.ts: export type TokenPresence',
  );
});

test('nothing under src/ipc/mock is token-shaped', async () => {
  expect(await hits(join(import.meta.dir, 'mock'), '**/*', TOKEN_SHAPED)).toEqual([]);
});

test('no generated IPC type has a string field named like a secret', async () => {
  expect(await hits(join(import.meta.dir, 'generated'), '**/*.ts', SECRET_FIELD)).toEqual([]);
});
