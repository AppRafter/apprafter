// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The hint under the wizard's token field. The rule is D.3d's (rules.ts, held to the core); this
// only says it as the owner types: where to get a token, the count so far, or what is wrong.
import { HETZNER_TOKEN_LEN } from '../../ipc/generated/target';
import { tokenMessage, tokenProblem } from '../targets/rules';

const utf8 = new TextEncoder();
const bytes = (s: string) => utf8.encode(s).length;

export function tokenHint(token: string): string {
  if (token === '') return 'Cloud Console → Security → API Tokens, read & write.';
  if (tokenProblem(token) === 'not_alphanumeric') return tokenMessage(token) ?? '';
  return `${bytes(token)}/${HETZNER_TOKEN_LEN} characters`;
}
