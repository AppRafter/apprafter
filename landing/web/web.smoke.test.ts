// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// landing/web smoke — lock down the Astro scaffold + design-token
// invariants so an accidental delete or drift is caught in CI before
// it lands on apprafter.dev.

import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = import.meta.dir;

describe('landing/web scaffold', () => {
  test('astro + svelte configs present', () => {
    expect(existsSync(join(ROOT, 'astro.config.ts'))).toBe(true);
    expect(existsSync(join(ROOT, 'svelte.config.js'))).toBe(true);
  });

  test('self-hosted Roboto WOFF2 files committed', () => {
    for (const w of ['400', '500', '700']) {
      expect(existsSync(join(ROOT, `public/fonts/roboto-${w}.woff2`))).toBe(true);
    }
    for (const w of ['400', '500']) {
      expect(existsSync(join(ROOT, `public/fonts/roboto-mono-${w}.woff2`))).toBe(true);
    }
  });

  test('Apache-2.0 attribution for Roboto present', () => {
    const license = readFileSync(join(ROOT, 'public/fonts/LICENSE-Roboto.txt'), 'utf8');
    expect(license).toContain('Apache License');
  });

  test('design tokens include accent + dark/light theme split', () => {
    const tokens = readFileSync(join(ROOT, 'src/styles/tokens.css'), 'utf8');
    expect(tokens).toContain(':root');
    // tokens.css aligns colons — match accent regardless of intervening whitespace.
    expect(tokens).toMatch(/--accent:\s+#14b8a6/);
    expect(tokens).toContain("[data-theme='light']");
    expect(tokens).toMatch(/--bg:\s+#0a0e1a/);
    expect(tokens).toMatch(/--bg:\s+#fafafa/);
  });

  test('theme init script flips data-theme on <html>', () => {
    const init = readFileSync(join(ROOT, 'src/lib/theme-init.ts'), 'utf8');
    expect(init).toContain('THEME_INIT_SCRIPT');
    expect(init).toContain('data-theme');
    expect(init).toContain('apprafter-theme');
  });
});
