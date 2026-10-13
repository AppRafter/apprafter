// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import type { Os } from '../ipc/generated/Os';
import type { SecretsMode } from '../ipc/generated/SecretsMode';
import { secretCopy } from './secretCopy';

const OSES: readonly Os[] = ['linux', 'macos', 'windows'];

test("a protected store's token reads the same on every OS (D.4a D21)", () => {
  const protectedModes: readonly SecretsMode[] = ['locking', 'locked', 'unlocking'];
  for (const os of OSES) {
    for (const mode of protectedModes) {
      expect(secretCopy(os, mode)).toBe("Encrypted in this target store's vault");
    }
  }
});

test('a secret file is "readable only by you" only where the file mode makes it so', () => {
  expect(secretCopy('linux', 'files')).toBe('Saved in a file readable only by you');
  expect(secretCopy('macos', 'files')).toBe('Saved in a file readable only by you');
  // Before D.4 the file gets no owner-only ACL on Windows (spec §2 honesty).
  expect(secretCopy('windows', 'files')).toBe('Saved in a file in your user profile');
});
