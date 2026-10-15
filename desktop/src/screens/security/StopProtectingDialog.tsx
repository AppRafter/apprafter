// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Stop protecting secrets (D.4a §3.14, T37, T63, O13; `apprafter lock off`), or finishing its
// interrupted run: a destructive plan whose gesture is the store's lock password or recovery
// key, asked even though the app is unlocked (O11), never a system prompt. It says the secrets
// will be on disk in clear again; where the plan re-seals a cache (`needsKey`, d4a-f Task 3),
// also where the caches go and that the app does not read `APPRAFTER_AGE_KEY` (O13 is the
// command line's) — never a key that is not used.
import { PlanConfirm } from '../../components/PlanConfirm';
import type { AuthInfo } from '../../ipc/generated/AuthInfo';
import type { PlanView } from '../../ipc/generated/PlanView';
import type { JsonValue } from '../../ipc/generated/serde_json/JsonValue';
import type { UiError } from '../../ipc/generated/UiError';
import {
  CLEAR_AGAIN,
  NOTHING_MOVES,
  STOP_BODY,
  STOP_BUTTON,
  STOP_FINISH_TITLE,
  STOP_KEY_NOTE,
  STOP_TITLE,
} from '../../state/lockText';

export interface StopProtectingDialogProps {
  /** `op_plan_lock_off`'s view: destructive, its gesture the store's secret when a vault is there. */
  readonly view: PlanView;
  /** The plan uses the shared age key — only to re-seal a cache (`LockOffPlan.needsKey`). */
  readonly needsKey: boolean;
  /** An interrupted Stop protecting is being finished. */
  readonly finishing: boolean;
  readonly auth: AuthInfo | null;
  readonly onDone: (result: JsonValue) => void;
  /** The run failed or was cancelled; `change`, the plan's title (PlanConfirm). */
  readonly onFailed: (error: UiError, change: string) => void;
  readonly onClose: () => void;
}

export function StopProtectingDialog(p: StopProtectingDialogProps) {
  return (
    <PlanConfirm
      view={p.view}
      title={p.finishing ? STOP_FINISH_TITLE : STOP_TITLE}
      body={
        // No vault to open (a plain confirm, Task 17): nothing moves into clear, so neither
        // warning is said (UI verifier minor 2).
        p.view.gesture === 'none' ? (
          <p>{NOTHING_MOVES}</p>
        ) : (
          <>
            <p>{CLEAR_AGAIN}</p>
            <p>{STOP_BODY}</p>
            {p.needsKey && <p>{STOP_KEY_NOTE}</p>}
          </>
        )
      }
      confirmLabel={STOP_BUTTON}
      auth={p.auth}
      onDone={p.onDone}
      onFailed={p.onFailed}
      onClose={p.onClose}
    />
  );
}
