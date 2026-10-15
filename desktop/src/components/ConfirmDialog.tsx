// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Confirmation by plan class (spec §4.4): reversible runs without a dialog (needsDialog);
// bounded is a plain confirm; destructive shows the plan and, for remove/destroy, a type-the-name
// guard against a misclick. The OS gesture is not asked here: Rust asks it inside op_execute,
// so the dialog only says which prompt comes next — except where the OS cannot prompt
// (AuthInfo.passwordField): there the dialog has its own password field, whose value onConfirm
// hands to execute() and Rust checks. It refuses as the lock screen's does, and is shown the same
// way: the OS's words or a plain line, the back-off counted down from what Rust says, busy; the
// field is marked until the owner types again. The field is emptied after every answer; a wrong
// password leaves the plan waiting in Rust, so the owner simply tries again. A plan's own gesture
// (`PlanView.gesture`) decides when it is given; `store_secret` is the target store's lock password
// or recovery key, in the dialog's own field on every OS (D.4a T76).
import { type FormEvent, type ReactNode, useEffect, useId, useRef, useState } from 'react';
import { uiErrorOf } from '../ipc/api';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import { CORE_ERROR_CODES } from '../ipc/generated/core-errors';
import { DESKTOP_ERROR_CODES } from '../ipc/generated/errors';
import type { Gesture } from '../ipc/generated/Gesture';
import type { PlanClass } from '../ipc/generated/PlanClass';
import type { UiError } from '../ipc/generated/UiError';
import { authPrompt, authRefusal, retryLine, useRetryCountdown } from '../state/auth';
import { STORE_SECRET_FIELD } from '../state/lockText';
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
  /**
   * What confirming asks, as the plan names it (`PlanView.gesture`, D.4a §3.14). Given, it
   * decides and `osGesture` is not read: `os` the OS's gesture (or the dialog's own system
   * password field where the OS cannot prompt), `store_secret` the target store's lock password
   * or recovery key in the dialog's own field, which Rust checks against the vault, `none`
   * nothing.
   */
  gesture?: Gesture;
  /** AppInfo.auth: names the prompt, or asks for the password here (`passwordField`). */
  auth: AuthInfo | null;
  /**
   * The dialog closes once this resolves; a rejection is shown inline and it stays open.
   * `password`: given only when the dialog asked for it (a gesture, where the OS cannot
   * prompt), for execute(). After a wrong password (`auth_failed`), a busy prompt, a gesture
   * asked the way that is not there (no polkit agent, or polkit refusing outside an active
   * session: the field appears; the field where the OS prompts: the field goes), or an expired password (changed in the system meanwhile) Rust
   * keeps the plan, so the owner tries again in place and this runs the same plan (the same
   * opId) again.
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
  gesture,
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
  const storeSecret = gesture === 'store_secret';
  const osAsked = gesture === undefined ? osGesture : gesture === 'os';
  const field = storeSecret || (osAsked && method !== null && auth?.passwordField === true);
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
      let backoffStarts = false;
      // A mistyped recovery key in the store's field is a try the owner makes again, said in
      // the words the Protect dialog uses (D.4a critic A finding 26).
      const mistypedKey =
        storeSecret && refused.code === CORE_ERROR_CODES.SECRETS_RECOVERY_KEY_INVALID;
      if (AUTH_REFUSALS.has(refused.code) || mistypedKey) {
        const said = authRefusal(refused, field, storeSecret ? 'store' : 'system');
        setRefusal(said.lines);
        if (said.retryInMs !== null) {
          startRetry(said.retryInMs);
          backoffStarts = true;
        }
      } else {
        setError(refused);
      }
      // The field is emptied (below), so Confirm is unready again: disabled, and a browser drops
      // the focus of a control it disables onto the page. The owner types again in the field —
      // unless a back-off disables that too: then Confirm keeps the focus, waiting it out.
      if (field && !backoffStarts) passwordInput.current?.focus();
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
              label={storeSecret ? STORE_SECRET_FIELD : 'System password'}
              value={password}
              onChange={(value) => {
                setPassword(value);
                // The refusal was about the last password: typing a new one ends it.
                setRefusal((lines) => (lines.length === 0 ? lines : []));
              }}
              mono={false}
              readOnly={busy}
              disabled={backoff}
              aria-invalid={refusal.length > 0 || undefined}
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
          {osAsked && method !== null && !field && (
            <span className="confirm-gesture">{`Confirm with ${authPrompt(method)} next.`}</span>
          )}
          <Button size={32} onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          {/* While onConfirm runs, or a back-off counts down, it waits, focusable: it has the
              focus it was pressed with, and a browser drops the focus of a control it disables
              onto the page, out of reach of Esc and Tab. */}
          <Button
            type="submit"
            size={32}
            variant={danger ? 'danger-solid' : 'primary'}
            disabled={!ready && !busy && !backoff}
            pending={busy || backoff}
          >
            {confirmLabel}
          </Button>
        </div>
      </form>
    </ModalFrame>
  );
}
