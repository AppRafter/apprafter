// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Finish an interrupted Protect (D.4a §3.9, rule 4; team decision 2026-10-13): its vault is
// written, so finishing opens it with both the lock password and the recovery key — each as its
// own kind of slot — before any more secrets leave their files. Two password fields, emptied on
// every answer whatever it is (T78, critic B finding 17), then the plan's plain confirm
// (bounded). Without one of them, Stop protecting backs out and Protect starts over. The run's
// failure is said as Stop protecting says its own (WI-484): in the dialog's place, its message,
// help and code, until the owner closes it; a lock while it shows keeps it for after the unlock
// (state/shownFailure.ts).
import { type FormEvent, useRef, useState } from 'react';
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { Modal } from '../../components/Modal';
import { PasswordField } from '../../components/PasswordField';
import { PlanConfirm } from '../../components/PlanConfirm';
import { useToast } from '../../components/Toast';
import { uiErrorOf } from '../../ipc/api';
import type { AuthInfo } from '../../ipc/generated/AuthInfo';
import { CORE_ERROR_CODES } from '../../ipc/generated/core-errors';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import type { PlanView } from '../../ipc/generated/PlanView';
import type { UiError } from '../../ipc/generated/UiError';
import { holdPlan } from '../../ipc/heldPlans';
import { authRefusal, retryLine, useRetryCountdown } from '../../state/auth';
import {
  FINISH_BUTTON,
  FINISH_CONTINUE,
  FINISH_TITLE,
  finishIntro,
  PASSWORD_FIELD,
  protectedSummary,
  protectOutcome,
  RECOVERY_FIELD,
} from '../../state/lockText';
import { useScope } from '../../state/scope';
import { useShownFailure } from '../../state/shownFailure';

export interface FinishProtectDialogProps {
  readonly store: string;
  readonly auth: AuthInfo | null;
  /** `op_plan_lock_finish`: Rust opens both slots before it plans. */
  readonly plan: (password: string, recovery: string) => Promise<PlanView>;
  readonly onDone: () => void;
  readonly onClose: () => void;
}

/** The refusals said in the store's words (a mistyped key's group; the back-off's countdown). */
const STORE_REFUSALS = new Set<string | null>([
  DESKTOP_ERROR_CODES.AUTH_FAILED,
  DESKTOP_ERROR_CODES.AUTH_BUSY,
  CORE_ERROR_CODES.SECRETS_RECOVERY_KEY_INVALID,
]);

/** A refusal of the two secrets in the store's words, and the back-off it starts, if any. */
function inWords(error: UiError): {
  readonly error: UiError | null;
  readonly retryInMs: number | null;
} {
  if (!STORE_REFUSALS.has(error.code)) return { error, retryInMs: null };
  const said = authRefusal(error, true, 'store');
  // Turned away by the back-off, nothing was checked: the countdown alone says so.
  if (said.lines.length === 0 && said.retryInMs !== null) {
    return { error: null, retryInMs: said.retryInMs };
  }
  return {
    error: { ...error, message: said.lines.join(' ') || error.message, help: null },
    retryInMs: said.retryInMs,
  };
}

export function FinishProtectDialog(p: FinishProtectDialogProps) {
  const scope = useScope();
  const toast = useToast();
  const [password, setPassword] = useState('');
  const [recovery, setRecovery] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<UiError | null>(null);
  const [view, setView] = useState<PlanView | null>(null);
  const [retryIn, startRetry] = useRetryCountdown();
  // The run's failure, in the dialog's place (WI-484).
  const [failure, showFailure] = useShownFailure();
  // The run failed: the confirm's own close that follows leaves the failure shown.
  const failed = useRef(false);
  if (failure !== null) {
    return (
      <Modal title={FINISH_TITLE} width={520} onClose={p.onClose}>
        <ErrorPanel error={failure.error} />
      </Modal>
    );
  }
  if (view !== null) {
    return (
      <PlanConfirm
        view={view}
        title={FINISH_TITLE}
        confirmLabel={FINISH_BUTTON}
        auth={p.auth}
        onDone={(result) => {
          // What really happened: another run first, a token left in clear (UI verifier M3).
          toast({ message: protectedSummary(protectOutcome(result), true), end: 'done' });
          p.onDone();
        }}
        onFailed={(error, change) => {
          failed.current = true;
          showFailure(error, change);
        }}
        onClose={() => {
          // A run that failed shows why, in the dialog's place.
          if (!failed.current) p.onClose();
        }}
      />
    );
  }
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (password === '' || recovery === '' || busy) return;
    const sent = { password, recovery };
    // T78: both fields are emptied on every answer, whatever it is.
    setPassword('');
    setRecovery('');
    setError(null);
    setBusy(true);
    try {
      const got = await p.plan(sent.password, sent.recovery);
      holdPlan(scope, got.opId);
      setView(got);
    } catch (reason) {
      const said = inWords(uiErrorOf(reason));
      setError(said.error);
      if (said.retryInMs !== null) startRetry(said.retryInMs);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal title={FINISH_TITLE} width={520} onClose={p.onClose}>
      <form
        className="lock-finish"
        onSubmit={(event) => {
          void submit(event);
        }}
      >
        <p>{finishIntro(p.store)}</p>
        <PasswordField
          label={PASSWORD_FIELD}
          value={password}
          onChange={setPassword}
          mono={false}
          readOnly={busy}
        />
        <PasswordField
          label={RECOVERY_FIELD}
          value={recovery}
          onChange={setRecovery}
          readOnly={busy}
        />
        {error !== null && <ErrorPanel error={error} />}
        {/* The back-off, counted down as the lock screen counts it (UI verifier minor 3). */}
        {retryIn !== null && <p className="lock-line">{retryLine(retryIn)}</p>}
        <Button
          type="submit"
          variant="primary"
          size={28}
          disabled={busy || retryIn !== null || password === '' || recovery === ''}
        >
          {FINISH_CONTINUE}
        </Button>
      </form>
    </Modal>
  );
}
