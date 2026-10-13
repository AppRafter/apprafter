// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Where a target's token is kept, in the words the Target screen and the wizard use. Honest per
// OS (spec §2): the file backend is owner-only by its mode on Linux and macOS; on Windows it gets
// no owner-only ACL before D.4, so it is not called "readable only by you" there. A store whose
// secrets are protected keeps the token in its vault, on every OS (D.4a surface D21).
import type { Os } from '../ipc/generated/Os';
import type { SecretsMode } from '../ipc/generated/SecretsMode';

export function secretCopy(os: Os, mode: SecretsMode): string {
  switch (mode) {
    case 'files':
      return os === 'windows'
        ? 'Saved in a file in your user profile'
        : 'Saved in a file readable only by you';
    case 'locking':
    case 'locked':
    case 'unlocking':
      return "Encrypted in this target store's vault";
  }
}
