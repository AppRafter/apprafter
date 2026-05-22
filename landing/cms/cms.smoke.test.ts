// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// landing/cms smoke — lock down the Payload + Next routing scaffold.
// Phase H expands collections and globals; this file should grow with
// new assertions for each addition (one assertion per slug is the goal).

import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = import.meta.dir;

describe('landing/cms scaffold', () => {
  test('payload + next configs present', () => {
    expect(existsSync(join(ROOT, 'src/payload.config.ts'))).toBe(true);
    expect(existsSync(join(ROOT, 'next.config.mjs'))).toBe(true);
  });

  test('Payload (payload) route group exposes admin and API endpoints', () => {
    const routes = [
      'src/app/(payload)/layout.tsx',
      'src/app/(payload)/admin/[[...segments]]/page.tsx',
      'src/app/(payload)/admin/[[...segments]]/not-found.tsx',
      'src/app/(payload)/admin/importMap.js',
      'src/app/(payload)/api/[...slug]/route.ts',
      'src/app/(payload)/api/graphql/route.ts',
      'src/app/(payload)/api/graphql-playground/route.ts',
    ];
    for (const r of routes) {
      expect(existsSync(join(ROOT, r))).toBe(true);
    }
  });

  test('Postgres adapter wired into payload.config', () => {
    const cfg = readFileSync(join(ROOT, 'src/payload.config.ts'), 'utf8');
    expect(cfg).toContain('@payloadcms/db-postgres');
    expect(cfg).toContain('postgresAdapter');
  });

  test('Users collection (default-auth admin) registered', () => {
    const usersPath = join(ROOT, 'src/collections/Users.ts');
    expect(existsSync(usersPath)).toBe(true);
    const users = readFileSync(usersPath, 'utf8');
    expect(users).toContain('auth: true');
  });

  test('Next root / redirects to /admin (no public frontend on the cms host)', () => {
    const nextCfg = readFileSync(join(ROOT, 'next.config.mjs'), 'utf8');
    expect(nextCfg).toContain('/admin');
    expect(nextCfg).toContain('redirects');
  });
});
