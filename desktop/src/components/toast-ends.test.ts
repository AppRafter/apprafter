// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// D.4a N5: a toast that says how a change ended is marked `end`, so a lock while it is still
// shown keeps it for after the unlock (Toast.tsx). A toast added without the mark would be lost
// to a lock again: every `toast({…})` outside the tests carries `end:`, or is one of the toasts
// below, which report no change's end and a lock rightly takes away.
import { expect, test } from 'bun:test';
import { join } from 'node:path';

/** The toasts that are no change's end, by their message, each with why. */
const NO_END: readonly string[] = [
  // The recovery key copied: a lock clears the clipboard, so saying it again would be false.
  'message: COPIED',
  // Text copied, or not (state/copy.ts): no change was made.
  'message: done',
  'message: `Not copied: ',
  // The toolchain's install page could not be opened: no change was made.
  'message: `Not opened: ',
  // Lock now refused while the secrets are in files: nothing locked, nothing changed.
  'message: lockOffMessage(off)',
  // A setting Rust refused (state/settings.ts): the view already shows the setting that holds,
  // and the refusal may be the lock's own, which the unlock has answered.
  'message: uiErrorOf(error).message',
];

const desktop = join(import.meta.dir, '..', '..');

/** Each `toast({…})` call's argument in `text`, from its `{` to the brace that closes it. */
function toastCalls(text: string): string[] {
  const calls: string[] = [];
  for (let at = text.indexOf('toast({'); at !== -1; at = text.indexOf('toast({', at + 1)) {
    const open = at + 'toast('.length;
    let depth = 0;
    for (let i = open; i < text.length; i += 1) {
      if (text[i] === '{') depth += 1;
      else if (text[i] === '}') depth -= 1;
      if (depth === 0) {
        calls.push(text.slice(open, i + 1));
        break;
      }
    }
  }
  return calls;
}

test('D.4a N5: every toast outside the tests says whether it is a change’s end', async () => {
  const unmarked: string[] = [];
  const seen = new Set<string>();
  for await (const path of new Bun.Glob('src/**/*.{ts,tsx}').scan({ cwd: desktop })) {
    if (/\.test\.tsx?$/.test(path)) continue;
    for (const call of toastCalls(await Bun.file(join(desktop, path)).text())) {
      const exempt = NO_END.find((message) => call.includes(message));
      if (exempt !== undefined) seen.add(exempt);
      else if (!/\bend: /.test(call)) unmarked.push(`${path}: toast(${call})`);
    }
  }
  expect(unmarked).toEqual([]);
  // Each exemption still names a toast: one left behind would exempt the next with its words.
  expect([...seen].sort()).toEqual([...NO_END].sort());
});
