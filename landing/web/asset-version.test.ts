// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Lives at the web root (not under src/) so astro check — which type-checks
// `src` against the strict tsconfig — never sees the `bun:test` import.

import { describe, expect, test } from 'bun:test';
import { assetExists, assetVersion, shortHash } from './src/lib/asset-version';

describe('shortHash', () => {
  test('returns the first 8 hex chars of the content sha256', () => {
    // sha256('hello') = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
    expect(shortHash('hello')).toBe('2cf24dba');
  });

  test('is deterministic for identical content', () => {
    expect(shortHash('apprafter')).toBe(shortHash('apprafter'));
  });

  test('differs when content differs', () => {
    expect(shortHash('a')).not.toBe(shortHash('b'));
  });
});

describe('assetVersion', () => {
  test('hashes the bytes of a real public/ asset to 8 lowercase hex', () => {
    expect(assetVersion('favicon.svg')).toMatch(/^[0-9a-f]{8}$/);
  });

  test('throws a clear error naming the helper for a missing asset', () => {
    expect(() => assetVersion('does-not-exist.svg')).toThrow(/asset-version/i);
  });
});

describe('assetExists', () => {
  test('is true for a real public/ asset', () => {
    expect(assetExists('favicon.svg')).toBe(true);
  });

  test('is false for a missing asset', () => {
    expect(assetExists('does-not-exist.svg')).toBe(false);
  });

  test('is false for a path escaping public/ even if the file exists', () => {
    // ../tsconfig.json exists at the web root but is outside public/.
    expect(assetExists('../tsconfig.json')).toBe(false);
  });
});

describe('containment + collision', () => {
  test('assetVersion refuses a path escaping public/', () => {
    expect(() => assetVersion('../tsconfig.json')).toThrow(/asset-version/i);
  });

  test('assetVersion differs for two distinct real assets', () => {
    expect(assetVersion('favicon.svg')).not.toBe(assetVersion('favicon-16.png'));
  });
});
