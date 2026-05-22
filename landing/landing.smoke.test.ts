// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Workspace-level smoke. The landing is a content-driven static site
// (per implementation-task.md §13 "No tests required for v1"), so this
// file does NOT cover application logic — instead it locks down the
// monorepo shape so a missing workspace, a missing shared config, or a
// silent move out of FSL-1.1-Apache-2.0 trips CI.

import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = import.meta.dir;

describe('landing workspace shape', () => {
  test('web/ child package present', () => {
    expect(existsSync(join(ROOT, 'web/package.json'))).toBe(true);
  });

  test('cms/ child package present', () => {
    expect(existsSync(join(ROOT, 'cms/package.json'))).toBe(true);
  });

  test('shared tsconfig.base.json extended by both children', () => {
    const base = join(ROOT, 'tsconfig.base.json');
    expect(existsSync(base)).toBe(true);
    for (const child of ['web', 'cms']) {
      const childTs = JSON.parse(readFileSync(join(ROOT, child, 'tsconfig.json'), 'utf8'));
      const extendsField = Array.isArray(childTs.extends) ? childTs.extends : [childTs.extends];
      expect(extendsField.some((p: string) => p?.includes('tsconfig.base.json'))).toBe(true);
    }
  });

  test('docker-compose Postgres for cms dev is committed', () => {
    expect(existsSync(join(ROOT, 'docker-compose.yml'))).toBe(true);
  });

  test('design source-of-truth files snapshotted under docs/design-source/', () => {
    for (const f of ['LANDING_BRIEF_v2.2.md', 'sections.jsx', 'styles.css', 'logo.jsx']) {
      expect(existsSync(join(ROOT, 'docs/design-source', f))).toBe(true);
    }
  });

  test('SPDX-License-Identifier is FSL-1.1-Apache-2.0 in the per-landing checker', () => {
    const checker = readFileSync(join(ROOT, 'scripts/check-spdx.sh'), 'utf8');
    expect(checker).toContain('FSL-1.1-Apache-2.0');
  });
});
