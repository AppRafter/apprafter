// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What an SSH key file the core inspected is, in the Target screen's words: why it cannot be the
// target's key (the form's refusal), and what its row says beside the path. The core names a
// type only for an OpenSSH public key; a private key — one dropped `.pub` away from its public
// half — is refused by name and never saved (GOTCHA-149).
import type { SshKeyInfo } from '../../ipc/generated/SshKeyInfo';

/** Why the inspected file cannot be the target's key, as the form says it; null for a key. */
export function keyRefusal(info: SshKeyInfo): string | null {
  switch (info.problem) {
    case null:
      return null;
    case 'missing':
      return `No file at ${info.display}.`;
    case 'unreadable':
      return `${info.display} cannot be read.`;
    case 'private_key':
      return `${info.display} is a private key: choose its public half, the .pub file next to it.`;
    case 'not_public_key':
      return `${info.display} is not an SSH public key.`;
  }
}

/** The SSH key row's value: the path, its type, and what is wrong with it. */
export function keyValue(key: SshKeyInfo | null): string {
  if (key === null) return 'not set';
  const type = key.algo === null ? '' : ` · ${key.algo}`;
  switch (key.problem) {
    case null:
      return `${key.display}${type}`;
    case 'missing':
      return `${key.display} (missing)`;
    case 'unreadable':
      return `${key.display} (cannot be read)`;
    case 'private_key':
      return `${key.display} (a private key: not used)`;
    case 'not_public_key':
      return `${key.display} (not a public key)`;
  }
}
