// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The core's target-name and token rules, so a form says what is wrong before it asks Rust; Rust
// checks again and decides. fixtures/target-names.json holds the name rule equal to the core's.
import { HETZNER_TOKEN_LEN, TARGET_NAME_MAX_LEN } from '../../ipc/generated/target';

/** The core's NameProblem, as `UiError.fields.problem` carries it. */
export type NameProblem = 'empty' | 'too_long' | 'invalid_char' | 'edge_dash';
/** The core's TokenProblem, likewise. */
export type TokenProblem = 'wrong_length' | 'not_alphanumeric';

const utf8 = new TextEncoder();
const bytes = (text: string) => utf8.encode(text).length;

/** apprafter_core::target::validate_name, in its order: empty, length (bytes), characters, dashes. */
export function nameProblem(name: string): NameProblem | null {
  if (name === '') return 'empty';
  if (bytes(name) > TARGET_NAME_MAX_LEN) return 'too_long';
  if (!/^[A-Za-z0-9-]+$/.test(name)) return 'invalid_char';
  if (name.startsWith('-') || name.endsWith('-')) return 'edge_dash';
  return null;
}

export function nameMessage(problem: NameProblem | null): string | null {
  switch (problem) {
    case null:
      return null;
    case 'empty':
      return 'Enter a name.';
    case 'too_long':
      return `At most ${TARGET_NAME_MAX_LEN} characters.`;
    case 'invalid_char':
      return 'Letters, digits and dashes only.';
    case 'edge_dash':
      return 'No dash at the start or the end.';
  }
}

/** cli_core::validate_hetzner_token_format: the length (bytes) first, then ASCII letters and digits. */
export function tokenProblem(token: string): TokenProblem | null {
  if (bytes(token) !== HETZNER_TOKEN_LEN) return 'wrong_length';
  if (!/^[A-Za-z0-9]+$/.test(token)) return 'not_alphanumeric';
  return null;
}

export function tokenMessage(token: string): string | null {
  switch (tokenProblem(token)) {
    case null:
      return null;
    case 'wrong_length':
      return `A Hetzner Cloud token is ${HETZNER_TOKEN_LEN} characters; this one has ${bytes(token)}.`;
    case 'not_alphanumeric':
      return 'Letters and digits only: no spaces, dashes or quotes.';
  }
}
