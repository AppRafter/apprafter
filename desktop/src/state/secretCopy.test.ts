// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import type { Os } from '../ipc/generated/Os';
import { secretCopy } from './secretCopy';

const OSES: readonly Os[] = ['linux', 'macos', 'windows'];

test("a protected store's token reads the same on every OS (D.4a D21), an interrupted one says so", () => {
  for (const os of OSES) {
    expect(secretCopy(os, 'locked')).toBe("Encrypted in this target store's vault");
    expect(secretCopy(os, 'locking')).toContain('protecting was interrupted');
    expect(secretCopy(os, 'unlocking')).toContain('stopping protection was interrupted');
    expect(secretCopy(os, 'locked', true)).not.toContain('Encrypted');
  }
});

test('a secret file is "readable only by you" only where the file mode makes it so', () => {
  expect(secretCopy('linux', 'files')).toBe('Saved in a file readable only by you');
  expect(secretCopy('macos', 'files')).toBe('Saved in a file readable only by you');
  // Before D.4 the file gets no owner-only ACL on Windows (spec §2 honesty).
  expect(secretCopy('windows', 'files')).toBe('Saved in a file in your user profile');
});
