// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The webview side of overview §6.3: the mock IPC holds nothing token-shaped (its fixtures end
// up in screenshots and docs), and no generated IPC type has a string field that could carry a
// token, a password or a secret to the page.
import { expect, test } from 'bun:test';
import { join } from 'node:path';

const TOKEN_SHAPED = /[A-Za-z0-9]{64}/;
/**
 * A property whose name holds token, password or secret anywhere — camelCase (`hetznerToken`),
 * snake_case (`hetzner_token`), quoted or not, any case (review #17: the word boundary let
 * `hetznerToken` through) — whose type has a `string` in it (`string | null`, `Array<string>`).
 */
const SECRET_FIELD =
  /["']?[A-Za-z0-9_]*(?:token|password|secret)[A-Za-z0-9_]*["']?\??\s*:\s*[^,;}\n]*\bstring\b/i;

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
  for (const leak of [
    '  token: string;',
    '  password?: string;',
    'hetznerToken: string | null, ',
    'hetzner_token: string',
    '"apiToken"?: string,',
    'HETZNER_TOKEN: string',
    'clientSecret: Array<string>, ',
    'accessTokens: string[];',
    'region: string, tokenValue: string, ',
  ]) {
    expect(leak).toMatch(SECRET_FIELD);
  }
  for (const fine of [
    '  token: TokenPresence;',
    'passwordField: boolean, };',
    'secretBackend: SecretBackend, account: string',
    'tokenChars: number | null,',
    '{ "kind": "renew_token", target: string, why: RenewWhy, }',
    'token: Verification | null, sshKeyChanged: boolean,',
  ]) {
    expect(fine).not.toMatch(SECRET_FIELD);
  }
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
