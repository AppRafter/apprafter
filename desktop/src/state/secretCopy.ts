// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Where a target's token is kept, in the words the Target screen and the wizard use. Honest per
// OS (spec §2): the file backend is owner-only by its mode on Linux and macOS; on Windows it gets
// no owner-only ACL before D.4, so it is not called "readable only by you" there.
import type { Os } from '../ipc/generated/Os';
import type { SecretBackend } from '../ipc/generated/SecretBackend';

export function secretCopy(os: Os, backend: SecretBackend): string {
  switch (backend) {
    case 'keyring':
      return 'Saved to the system keychain';
    case 'file':
      return os === 'windows'
        ? 'Saved in a file in your user profile'
        : 'Saved in a file readable only by you';
  }
}
