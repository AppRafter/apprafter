// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Where a target's token is kept, in the words the Target screen and the wizard use. Honest per
// OS (spec §2): the file backend is owner-only by its mode on Linux and macOS; on Windows it gets
// no owner-only ACL before D.4, so it is not called "readable only by you" there. A store whose
// secrets are protected keeps the token in its vault, on every OS (D.4a surface D21).
// An interrupted run, or a store whose lock state cannot be used, says so (D.4a critic B
// finding 8); so does a store other accounts can change, whose secrets the core does not use
// (WI-475; WI-476).
import type { Os } from '../ipc/generated/Os';
import type { SecretsMode } from '../ipc/generated/SecretsMode';
import type { StoreProblem } from './lockText';

export function secretCopy(os: Os, mode: SecretsMode, problem: StoreProblem | null = null): string {
  if (problem === 'not_private')
    return 'Unavailable while other accounts can change this target store';
  if (problem !== null) return "Unavailable until this target store's lock state is repaired";
  switch (mode) {
    case 'files':
      return os === 'windows'
        ? 'Saved in a file in your user profile'
        : 'Saved in a file readable only by you';
    case 'locked':
      return "Encrypted in this target store's vault";
    case 'locking':
      return "Being moved into this target store's vault: protecting was interrupted";
    case 'unlocking':
      return 'Being moved back to a file: stopping protection was interrupted';
  }
}
