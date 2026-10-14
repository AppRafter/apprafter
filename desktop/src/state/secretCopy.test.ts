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
    expect(secretCopy(os, 'locked', 'lock_state')).not.toContain('Encrypted');
  }
});

test('a secret file is "readable only by you" only where the file mode makes it so', () => {
  expect(secretCopy('linux', 'files')).toBe('Saved in a file readable only by you');
  expect(secretCopy('macos', 'files')).toBe('Saved in a file readable only by you');
  // Before D.4 the file gets no owner-only ACL on Windows (spec §2 honesty).
  expect(secretCopy('windows', 'files')).toBe('Saved in a file in your user profile');
});

// WI-476 (the controller's decision on critic M7): a store other accounts can change keeps no
// token the core would use, on every OS and in every state.
test('a store other accounts can change: the token is unavailable until that is fixed', () => {
  for (const os of OSES) {
    for (const mode of ['files', 'locking', 'locked', 'unlocking'] as const) {
      expect(secretCopy(os, mode, 'not_private')).toBe(
        'Unavailable while other accounts can change this target store',
      );
    }
  }
});
