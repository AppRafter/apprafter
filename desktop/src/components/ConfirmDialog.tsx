// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Confirmation by plan class (spec §4.4): reversible runs without a dialog (needsDialog);
// bounded is a plain confirm; destructive shows the plan and, for remove/destroy, a type-the-name
// guard against a misclick. The OS gesture is not asked here: Rust asks it inside op_execute,
// so the dialog only says which prompt comes next.
import { type ReactNode, useId, useState } from 'react';
import { uiErrorOf } from '../ipc/api';
import type { AuthInfo } from '../ipc/generated/AuthInfo';
import type { PlanClass } from '../ipc/generated/PlanClass';
import type { UiError } from '../ipc/generated/UiError';
import { authPrompt } from '../state/auth';
import { Button } from './Button';
import { ErrorPanel } from './ErrorPanel';
import { type Icon, QuestionIcon } from './icons';
import { ModalFrame } from './Modal';

/** Whether a plan of this class is confirmed in a dialog; a reversible one just runs. */
export function needsDialog(planClass: PlanClass): planClass is 'bounded' | 'destructive' {
  return planClass !== 'reversible';
}

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
  /** AppInfo.auth: names the prompt. */
  auth: AuthInfo | null;
  /** The dialog closes once this resolves; a rejection is shown inline and it stays open. */
  onConfirm: () => Promise<void> | void;
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
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<UiError | null>(null);
  const destructive = planClass === 'destructive';
  const guard = destructive ? requireText : undefined;
  const matches = guard === undefined || typed === guard;
  const method = auth?.available ? auth.method : null;

  const confirm = async () => {
    if (!matches || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
    } catch (reason) {
      setError(uiErrorOf(reason));
      setBusy(false);
      return;
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
      <div className="alert-form">
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
        {error !== null && <ErrorPanel error={error} />}
        <div className="alert-foot">
          {osGesture && method !== null && (
            <span className="confirm-gesture">{`Confirm with ${authPrompt(method)} next.`}</span>
          )}
          <Button size={32} onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            size={32}
            variant={danger ? 'danger-solid' : 'primary'}
            disabled={!matches || busy}
            onClick={confirm}
          >
            {confirmLabel}
          </Button>
        </div>
      </div>
    </ModalFrame>
  );
}
