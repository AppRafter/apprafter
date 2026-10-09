// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Confirmation by plan class (spec §4.4): reversible runs without a dialog (needsDialog);
// bounded is a plain confirm; destructive shows the plan and, for remove/destroy, a type-the-name
// guard against a misclick. The OS gesture is not asked here: Rust asks it inside op_execute,
// so the dialog only says which prompt comes next — except where the OS cannot prompt
// (AuthInfo.passwordField): there the dialog has its own password field, whose value onConfirm
// hands to execute() and Rust checks. It refuses as the lock screen's does, and is shown the same
// way: the OS's words or a plain line, the back-off counted down from what Rust says, busy. The
// field is emptied after every answer; a wrong password leaves the plan waiting in Rust, so the
// owner simply tries again.
import { type FormEvent, type ReactNode, useEffect, useId, useRef, useState } from 'react';
import { uiErrorOf } from '../ipc/api';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { PlanClass } from '../ipc/generated/PlanClass';
import type { UiError } from '../ipc/generated/UiError';
import { authPrompt, authRefusal, retryLine, useRetryCountdown } from '../state/auth';
import { Button } from './Button';
import { ErrorPanel } from './ErrorPanel';
import { type Icon, QuestionIcon } from './icons';
import { ModalFrame } from './Modal';
import { PasswordField } from './PasswordField';

/** Whether a plan of this class is confirmed in a dialog; a reversible one just runs. */
export function needsDialog(planClass: PlanClass): planClass is 'bounded' | 'destructive' {
  return planClass !== 'reversible';
}

/** The refusals said in plain lines, as the lock screen says them; the rest show as errors. */
const AUTH_REFUSALS: ReadonlySet<string | null> = new Set([
  DESKTOP_ERROR_CODES.AUTH_FAILED,
  DESKTOP_ERROR_CODES.AUTH_BUSY,
  DESKTOP_ERROR_CODES.AUTH_UNAVAILABLE,
]);

export interface ConfirmDialogProps {
  title: string;
  body: ReactNode;
  icon?: Icon;
  confirmLabel: string;
  danger?: boolean;
  planClass: 'bounded' | 'destructive';
  /** Destructive only: the changes the plan makes. */
  plan?: ReactNode;
  /** Destructive only: the text to type, exactly, before confirming (remove, destroy). */
  requireText?: string;
  /** Whether Rust asks the OS for the owner next; by default a destructive plan does. */
  osGesture?: boolean;
  /** AppInfo.auth: names the prompt, or asks for the password here (`passwordField`). */
  auth: AuthInfo | null;
  /**
   * The dialog closes once this resolves; a rejection is shown inline and it stays open.
   * `password`: given only when the dialog asked for it (a gesture, where the OS cannot
   * prompt), for execute(). After a wrong password (`auth_failed`), a busy prompt, or a gesture
   * asked the way that is not there (no polkit agent: the field appears; the field where the OS
   * prompts: the field goes) Rust keeps the plan, so the owner tries again in place and this
   * runs the same plan (the same opId) again.
   */
  onConfirm: (password?: string) => Promise<void> | void;
  onClose: () => void;
}

export function ConfirmDialog({
  title,
  body,
  icon: TitleIcon = QuestionIcon,
  confirmLabel,
  danger = false,
  planClass,
  plan,
  requireText,
  osGesture = planClass === 'destructive',
  auth,
  onConfirm,
  onClose,
}: ConfirmDialogProps) {
  const id = useId();
  const [typed, setTyped] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<UiError | null>(null);
  const [refusal, setRefusal] = useState<readonly string[]>([]);
  const [retryIn, startRetry] = useRetryCountdown();
  const backoff = retryIn !== null;
  const passwordInput = useRef<HTMLInputElement>(null);
  const destructive = planClass === 'destructive';
  const guard = destructive ? requireText : undefined;
  const method = auth?.available ? auth.method : null;
  const field = osGesture && method !== null && auth?.passwordField === true;
  const ready = (guard === undefined || typed === guard) && (!field || password !== '');

  // A field the back-off disabled lost the focus: it gets it back once the back-off ends.
  const wasBackoff = useRef(false);
  useEffect(() => {
    if (wasBackoff.current && !backoff) passwordInput.current?.focus();
    wasBackoff.current = backoff;
  }, [backoff]);

  const confirm = async (event?: FormEvent) => {
    event?.preventDefault();
    if (!ready || busy || backoff) return;
    setBusy(true);
    setError(null);
    setRefusal([]);
    try {
      await (field ? onConfirm(password) : onConfirm());
    } catch (reason) {
      const refused = uiErrorOf(reason);
      if (AUTH_REFUSALS.has(refused.code)) {
        const said = authRefusal(refused, field);
        setRefusal(said.lines);
        if (said.retryInMs !== null) startRetry(said.retryInMs);
      } else {
        setError(refused);
      }
      setBusy(false);
      return;
    } finally {
      setPassword('');
    }
    setBusy(false);
    onClose();
  };

  return (
    <ModalFrame
      labelledBy={`${id}-title`}
      describedBy={`${id}-body`}
      width={460}
      layer="confirm"
      variant="alert"
      onClose={onClose}
      closable={!busy}
      dismissOnBackdrop
    >
      <form className="alert-form" onSubmit={confirm}>
        <div className="alert-head">
          <TitleIcon className="alert-icon" data-danger={danger || undefined} aria-hidden="true" />
          <div className="alert-titles">
            <h2 className="alert-title" id={`${id}-title`}>
              {title}
            </h2>
            <div className="alert-body" id={`${id}-body`}>
              {body}
            </div>
          </div>
        </div>
        {destructive && plan !== undefined && <div className="confirm-plan">{plan}</div>}
        {guard !== undefined && (
          <div className="confirm-require" data-modal-body>
            <label htmlFor={`${id}-require`}>
              Type <span className="confirm-require-text">{guard}</span> to confirm
            </label>
            <input
              id={`${id}-require`}
              className="input"
              data-mono
              autoComplete="off"
              spellCheck={false}
              value={typed}
              onChange={(event) => setTyped(event.target.value)}
            />
          </div>
        )}
        {field && (
          <div className="confirm-password" data-modal-body>
            <PasswordField
              ref={passwordInput}
              label="System password"
              value={password}
              onChange={setPassword}
              mono={false}
              readOnly={busy}
              disabled={backoff}
            />
          </div>
        )}
        {(refusal.length > 0 || backoff) && (
          <div className="confirm-refusal" role="alert">
            {refusal.map((line, index) => (
              // A fixed list per answer, replaced whole: the index is a stable key.
              // biome-ignore lint/suspicious/noArrayIndexKey: see above
              <p key={index}>{line}</p>
            ))}
            {/* Announced with the refusal, not again every second. */}
            {backoff && <p aria-live="off">{retryLine(retryIn)}</p>}
          </div>
        )}
        {error !== null && <ErrorPanel error={error} />}
        <div className="alert-foot">
          {osGesture && method !== null && !field && (
            <span className="confirm-gesture">{`Confirm with ${authPrompt(method)} next.`}</span>
          )}
          <Button size={32} onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            type="submit"
            size={32}
            variant={danger ? 'danger-solid' : 'primary'}
            disabled={!ready || busy || backoff}
          >
            {confirmLabel}
          </Button>
        </div>
      </form>
    </ModalFrame>
  );
}
