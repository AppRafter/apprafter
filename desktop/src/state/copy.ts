// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Copy out, with a toast either way: what was copied, or that it was not and why, in the system's
// words (a session with no clipboard, a compositor without the protocol it needs). Write-only:
// nothing is ever read back (ipc/clipboard.ts).
import { useCallback } from 'react';
import { WarningCircleIcon } from '../components/icons';
import { useToast } from '../components/Toast';
import { uiErrorOf } from '../ipc/api';
import { copyText } from '../ipc/clipboard';

/** Copies `text`; the toast says `done`, or "Not copied: <why>". */
export function useCopy(): (text: string, done: string) => void {
  const toast = useToast();
  return useCallback(
    (text: string, done: string) => {
      copyText(text).then(
        () => toast({ message: done }),
        (reason: unknown) =>
          toast({ message: `Not copied: ${uiErrorOf(reason).message}`, icon: WarningCircleIcon }),
      );
    },
    [toast],
  );
}
