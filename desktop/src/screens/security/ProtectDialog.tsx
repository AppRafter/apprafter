// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Protect secrets (D.4a §3.14, T25, O2, O8, O12; `apprafter lock on`), five steps in the wizard
// frame: (1) what it does, what it stops and what it does not (honest + separate user); (2) the
// lock password twice, warned below 15 characters and never refused for its length (O2); (3) the
// recovery key, shown once, its Copy done by Rust and marked to stay out of clipboard history
// (O12, T81) — the page offers no copy of its own: the key's block cannot be selected, and a copy
// or cut of it is refused (ADR 0068 §3); "I saved it" asks Rust to take its copy off the
// clipboard and copy no more, so (4) the key, typed back in full in a password field, comes from
// where the person saved it, never from the app's own Copy (critic B finding 3); Rust checks it
// before anything runs (T25, C5), the plan's changes beside the button that confirms them; (5)
// the run, Cancel offered until it starts re-encrypting caches (§3.9). From step 2 the plan
// holds the password and the key in Rust; the page keeps the key's text only while this dialog
// can show it again (T79), and empties the confirm field before each check. The plan is held by
// the screen's scope (heldPlans): Back past the key, Close and the lock discard it, and with it
// what it holds. The run's failure, said in the dialog, is kept for after the unlock when a lock
// comes while it shows (WI-483, state/shownFailure.ts); a refusal before the run is not.
import { type ReactNode, type SyntheticEvent, useRef, useState } from 'react';
import { Button } from '../../components/Button';
import { ErrorPanel } from '../../components/ErrorPanel';
import { PasswordField } from '../../components/PasswordField';
import { PlanChanges } from '../../components/PlanChanges';
import { useToast } from '../../components/Toast';
import { Wizard } from '../../components/Wizard';
import * as api from '../../ipc/api';
import { uiErrorOf } from '../../ipc/api';
import { CORE_ERROR_CODES } from '../../ipc/generated/core-errors';
import type { LockOnPlan } from '../../ipc/generated/LockOnPlan';
import type { OpId } from '../../ipc/generated/OpId';
import type { UiError } from '../../ipc/generated/UiError';
import { holdPlan, releasePlan } from '../../ipc/heldPlans';
import { type Stage, useOperation } from '../../ipc/operations';
import { failureOf, isCancelled, resultOf, startPlan } from '../../ipc/plans';
import {
  AGENT_NOTE,
  AT_REST,
  CONFIRM_FIELD,
  COPIED,
  COPY_GONE,
  COPY_LABEL,
  noReset,
  PASSWORD_FIELD,
  PASSWORD_HINT,
  PASSWORDS_DIFFER,
  PROTECT_BUTTON,
  PROTECT_CANCELLED,
  PROTECT_STEPS,
  PROTECT_TITLE,
  protectedSummary,
  protectIntro,
  protectOutcome,
  RECOVERY_BODY,
  RECOVERY_SHAPED,
  RECOVERY_TITLE,
  REPEAT_FIELD,
  RESTART_NOTE,
  recoveryKeyLine,
  SAVED_IT,
  SHOW_AGAIN,
  STARTING,
  shortWarning,
  TYPE_A_PASSWORD,
} from '../../state/lockText';
import { useScope } from '../../state/scope';
import { useShownFailure } from '../../state/shownFailure';

export interface ProtectDialogProps {
  /** The store's root as the app shows it (`~/.config/apprafter`). */
  readonly store: string;
  /** An earlier Protect stopped before it wrote the vault: its recovery key is void (G13). */
  readonly restart: boolean;
  /** `op_plan_lock_on`: the plan, and the recovery key to show once. */
  readonly plan: (password: string) => Promise<LockOnPlan>;
  /**
   * `op_recovery_key_saved`: Rust takes its copy off the clipboard, if it is still there, and
   * copies the key no more (O12, critic B finding 3).
   */
  readonly keySaved: (opId: OpId) => Promise<void>;
  /** `op_confirm_recovery_key`: Rust compares the whole key, typed back (T25). */
  readonly confirmKey: (opId: OpId, typed: string) => Promise<void>;
  /** `copy_recovery_key`: Rust copies the key it holds (O12). */
  readonly copyKey: (opId: OpId) => Promise<void>;
  /** It ran (or was cancelled): what the Security page shows may have changed. */
  readonly onDone: () => void;
  readonly onClose: () => void;
}

/** §3.9: Cancel is honoured until the run's third stage, "Re-encrypting cached secrets", starts. */
export function cancellableAt(stage: Stage | null): boolean {
  return stage === null || stage.index < 3;
}

/** A refusal the page says under its field, or `null` for one shown as an error. */
function fieldLine(error: UiError): string | null {
  if (error.code === CORE_ERROR_CODES.SECRETS_PASSWORD_REFUSED) {
    if (error.fields.reason === 'empty') return TYPE_A_PASSWORD;
    if (error.fields.reason === 'recovery_key_shape') return RECOVERY_SHAPED;
    return error.message;
  }
  if (error.code === CORE_ERROR_CODES.SECRETS_RECOVERY_KEY_INVALID) {
    return recoveryKeyLine(error.fields) ?? error.message;
  }
  return null;
}

/** No operation yet: `useOperation` of an id Rust never gives. */
const NO_OP: OpId = -1;

/** The web view offers no copy of the key (ADR 0068 §3, O12): only Rust's Copy. */
const noCopy = (event: SyntheticEvent) => event.preventDefault();

export function ProtectDialog(p: ProtectDialogProps) {
  const scope = useScope();
  const toast = useToast();
  const [step, setStep] = useState(0);
  const [saved, setSaved] = useState(false);
  const [password, setPassword] = useState('');
  const [repeat, setRepeat] = useState('');
  const [typed, setTyped] = useState('');
  const [line, setLine] = useState<string | null>(null);
  const [failure, showFailure] = useShownFailure();
  const error = failure?.error ?? null;
  const [held, setHeld] = useState<LockOnPlan | null>(null);
  const [busy, setBusy] = useState(false);
  const ran = useRef(false);
  const opId = held?.plan.opId ?? NO_OP;
  const stage = useOperation(opId)?.stage ?? null;

  const said = () => {
    setLine(null);
    showFailure(null);
  };
  const fail = (reason: unknown) => {
    const ui = uiErrorOf(reason);
    const under = fieldLine(ui);
    if (under === null) showFailure(ui);
    else setLine(under);
  };
  /** The plan goes, and the password and key it holds in Rust with it; never once it ran. */
  const drop = () => {
    if (held === null || ran.current) return;
    const id = held.plan.opId;
    releasePlan(id);
    api.opDiscard(id).catch((e: unknown) => console.error(`op_discard ${id} failed:`, e));
    setHeld(null);
    setSaved(false);
  };
  const close = () => {
    drop();
    p.onClose();
  };

  const run = async (id: OpId) => {
    try {
      const started = await startPlan(id, undefined, {
        title: PROTECT_TITLE,
        shown: () => !scope.gone(),
      });
      ran.current = true;
      releasePlan(id);
      // What really happened: another run first, a token left in clear (UI verifier M3).
      toast({
        message: protectedSummary(protectOutcome(resultOf(await started.ended)), false),
        end: 'done',
      });
      p.onDone();
      p.onClose();
    } catch (reason) {
      if (isCancelled(reason)) {
        toast({ message: PROTECT_CANCELLED, end: 'failed' });
        p.onDone();
        p.onClose();
        return;
      }
      // A run that started and failed is a change's end; a start refused is not (nothing ran).
      showFailure(failureOf(reason), ran.current ? PROTECT_TITLE : undefined);
      setBusy(false);
    }
  };

  const next = async () => {
    said();
    if (step === 0) {
      setStep(1);
      return;
    }
    if (step === 2 && held !== null) {
      setBusy(true);
      try {
        await p.keySaved(held.plan.opId);
        setSaved(true);
        setStep(3);
      } catch (reason) {
        fail(reason);
      } finally {
        setBusy(false);
      }
      return;
    }
    if (step === 1) {
      if (password === '') {
        setLine(TYPE_A_PASSWORD);
        return;
      }
      if (password !== repeat) {
        setLine(PASSWORDS_DIFFER);
        return;
      }
      setBusy(true);
      try {
        const got = await p.plan(password);
        holdPlan(scope, got.plan.opId);
        setHeld(got);
        setPassword('');
        setRepeat('');
        setStep(2);
      } catch (reason) {
        fail(reason);
      } finally {
        setBusy(false);
      }
      return;
    }
    if (step === 3 && held !== null) {
      setBusy(true);
      // The typed key leaves the page's state before it is checked, whatever the answer.
      const sent = typed;
      setTyped('');
      try {
        await p.confirmKey(held.plan.opId, sent);
      } catch (reason) {
        fail(reason);
        setBusy(false);
        return;
      }
      setStep(4);
      await run(held.plan.opId);
    }
  };

  const back =
    step === 0 || step === 4
      ? undefined
      : () => {
          said();
          if (step === 2) drop();
          setStep(step - 1);
        };

  const warning = shortWarning(password);
  const steps: ReactNode[] = [
    <>
      <p>{protectIntro(p.store)}</p>
      {p.restart && <p data-tone="warn">{RESTART_NOTE}</p>}
      <p>{AGENT_NOTE}</p>
      <p>{AT_REST}</p>
    </>,
    <>
      <PasswordField
        label={PASSWORD_FIELD}
        value={password}
        onChange={(value) => {
          setPassword(value);
          said();
        }}
        hint={warning === null ? PASSWORD_HINT : <span data-tone="warn">{warning}</span>}
        mono={false}
        readOnly={busy}
      />
      <PasswordField
        label={REPEAT_FIELD}
        value={repeat}
        onChange={(value) => {
          setRepeat(value);
          said();
        }}
        mono={false}
        readOnly={busy}
      />
    </>,
    held !== null && (
      <>
        <p>{RECOVERY_BODY}</p>
        {/* A figure, not a <pre>: a <pre> takes no accessible name. Its CSS keeps the line break. */}
        <figure
          className="recovery-key"
          aria-label={RECOVERY_TITLE}
          onCopy={noCopy}
          onCut={noCopy}
          onContextMenu={noCopy}
        >
          {held.recoveryKey}
        </figure>
        {saved ? (
          <p>{COPY_GONE}</p>
        ) : (
          <Button
            size={28}
            onClick={() => {
              said();
              p.copyKey(held.plan.opId).then(
                () => toast({ message: COPIED }),
                (reason: unknown) => showFailure(uiErrorOf(reason)),
              );
            }}
          >
            {COPY_LABEL}
          </Button>
        )}
        <p>{noReset(p.store)}</p>
      </>
    ),
    held !== null && (
      <>
        <PasswordField
          label={CONFIRM_FIELD}
          value={typed}
          onChange={(value) => {
            setTyped(value);
            said();
          }}
          readOnly={busy}
        />
        <Button
          size={26}
          variant="ghost"
          onClick={() => {
            said();
            setStep(2);
          }}
        >
          {SHOW_AGAIN}
        </Button>
        <PlanChanges changes={held.plan.changes} />
      </>
    ),
    <div key="run" className="lock-run">
      <p role="status" aria-live="polite">
        {stage === null ? STARTING : `${stage.title}…`}
      </p>
      <Button
        size={28}
        disabled={!busy || !cancellableAt(stage)}
        onClick={() => {
          api.opCancel(opId).catch((e: unknown) => console.error(`op_cancel ${opId} failed:`, e));
        }}
      >
        Cancel
      </Button>
    </div>,
  ];

  return (
    <Wizard
      title={PROTECT_TITLE}
      steps={PROTECT_STEPS}
      step={step}
      {...(back !== undefined && { onBack: back })}
      nextLabel={
        ['Continue', 'Continue', SAVED_IT, PROTECT_BUTTON, PROTECT_BUTTON][step] ?? 'Continue'
      }
      nextDisabled={(step === 3 && typed.trim() === '') || (step === 4 && !busy)}
      busy={busy}
      onNext={() => {
        void next();
      }}
      onClose={close}
    >
      {steps[step]}
      {line !== null && (
        <p className="lock-line" role="alert">
          {line}
        </p>
      )}
      {error !== null && <ErrorPanel error={error} />}
    </Wizard>
  );
}
