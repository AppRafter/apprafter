// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import type { Os } from '../ipc/generated/Os';
import { secretCopy } from './secretCopy';

const OSES: readonly Os[] = ['linux', 'macos', 'windows'];

test('the keychain reads the same on every OS', () => {
  for (const os of OSES) expect(secretCopy(os, 'keyring')).toBe('Saved to the system keychain');
});

test('a secret file is "readable only by you" only where the file mode makes it so', () => {
  expect(secretCopy('linux', 'file')).toBe('Saved in a file readable only by you');
  expect(secretCopy('macos', 'file')).toBe('Saved in a file readable only by you');
  // Before D.4 the file gets no owner-only ACL on Windows (spec §2 honesty).
  expect(secretCopy('windows', 'file')).toBe('Saved in a file in your user profile');
});
