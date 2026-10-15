// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Shows the ends of confirmed plans whose screen was gone when they ended (ipc/away.ts): after an
// unlock, or right away when only a tab closed. One toast at a time, each given its time, and
// each operation discarded once its end is shown (a read's warning has none left to discard).
import { useEffect, useRef, useState } from 'react';
import { CheckCircleIcon, WarningCircleIcon } from '../components/icons';
import { TOAST_MS, useToast } from '../components/Toast';
import { takeEndedAway, useEndedAway } from '../ipc/away';
import { discard } from '../ipc/operations';
import { reportUnlessLocked } from '../ipc/plans';

export function EndedAwayNotices({ spacingMs = TOAST_MS }: { spacingMs?: number }) {
  const ended = useEndedAway();
  const toast = useToast();
  const showing = useRef(false);
  const [, setFree] = useState(0);
  const first = ended[0];
  useEffect(() => {
    if (first === undefined || showing.current) return;
    showing.current = true;
    takeEndedAway(first);
    // Itself an end: a lock while it shows keeps it again (D.4a N5).
    toast({
      message: first.text,
      icon: first.failed ? WarningCircleIcon : CheckCircleIcon,
      end: first.failed ? 'failed' : 'done',
    });
    const { opId } = first;
    if (opId !== null) discard(opId).catch(reportUnlessLocked(`op_discard ${opId}`));
    setTimeout(() => {
      showing.current = false;
      setFree((n) => n + 1);
    }, spacingMs);
  });
  return null;
}
