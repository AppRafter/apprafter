// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What the Targets page and the Target screen do to a target, each by its plan class (spec §4.4,
// decision 3): `use` is reversible and runs at once, without a dialog.
import { useQueryClient } from '@tanstack/react-query';
import { useCallback } from 'react';
import { useToast } from '../../components/Toast';
import * as api from '../../ipc/api';
import type { TargetUsed } from '../../ipc/generated/TargetUsed';
import type { UiError } from '../../ipc/generated/UiError';
import { OperationFailed, resultOf, startPlan } from '../../ipc/plans';
import { refreshTargets } from '../../state/targets';
import { usedMessage } from './outcomes';

/** Whatever a plan or its run was refused or failed with, as the UiError the caller shows. */
const refusalOf = (reason: unknown): UiError =>
  reason instanceof OperationFailed ? reason.error : api.uiErrorOf(reason);

/**
 * Make a target the CLI's default: the reversible `use` plan, run at once (no confirm). A plan
 * with nothing to change (it is the default already) is discarded unrun. Either way a toast says
 * what is true now and the list is read again; a refusal or a failure goes to `onFailed`.
 */
export function useMakeDefault(
  onFailed: (error: UiError) => void,
): (name: string) => Promise<void> {
  const client = useQueryClient();
  const toast = useToast();
  return useCallback(
    async (name: string) => {
      try {
        const view = await api.opPlanTargetUse(name);
        if (view.changes.length === 0) {
          await api.opDiscard(view.opId);
          toast({ message: usedMessage({ name, pointer: null }) });
        } else {
          const out = resultOf(await (await startPlan(view.opId)).ended) as unknown as TargetUsed;
          toast({ message: usedMessage(out) });
        }
        refreshTargets(client);
      } catch (reason) {
        onFailed(refusalOf(reason));
      }
    },
    [client, toast, onFailed],
  );
}
