// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The design's wizard frame (880 px): title, the stepper (a check for a step done), the body, and
// a footer with a mono hint, Back and the step's Next. The body and footer are one form, so Enter
// in a field goes next, unless Next is disabled. While busy (a verify or a save running),
// nothing closes it and Back and Next wait.
//
// The focus never falls out of the dialog. Each step is a new body (keyed by the step), and it
// takes the focus to its first control (a radio group's chosen radio), unless a control of the
// step took it already. While busy, the focus moves to the dialog if busy disabled the control
// that had it: a browser drops the focus of a control it disables onto the page, out of reach
// of Esc and Tab. When busy ends, a focus parked on the dialog, or lost to the page, goes back
// to the step. A control of the step that goes while it has the focus ("Use another token", a
// Try again replaced by what it started) hands it to the step's first control, or the dialog. A
// polite status says which step is shown.
//
// One activation is one Next: Next is one button for every step, so the second click of a double
// click, or a held Enter's repeat, would be the next step's Next (Save, after Continue) without
// the owner ever seeing that step. Both are dropped.
import { type FormEvent, type ReactNode, useId, useLayoutEffect, useRef } from 'react';
import { Button } from './Button';
import { IconButton } from './IconButton';
import { CheckIcon, XIcon } from './icons';
import { focusables, ModalFrame } from './Modal';

export interface WizardProps {
  readonly title: string;
  readonly steps?: readonly string[];
  /** The current step, 0-based. */
  readonly step?: number;
  readonly hint?: ReactNode;
  readonly onBack?: () => void;
  readonly nextLabel: string;
  readonly nextDisabled?: boolean;
  readonly busy?: boolean;
  readonly onNext: () => void;
  readonly onClose: () => void;
  readonly width?: number;
  readonly children: ReactNode;
}

/**
 * Where a step's focus starts: its first Tab stop. ModalFrame's focusables already count a radio
 * group as one stop, its chosen radio (the first enabled one when none is chosen).
 */
function firstStop(body: HTMLElement): HTMLElement | undefined {
  return focusables(body)[0];
}

export function Wizard({
  title,
  steps,
  step = 0,
  hint,
  onBack,
  nextLabel,
  nextDisabled = false,
  busy = false,
  onNext,
  onClose,
  width = 880,
  children,
}: WizardProps) {
  const id = useId();
  const bodyRef = useRef<HTMLDivElement>(null);
  const before = useRef({ step, busy });
  /** The control of the frame that last had the focus. */
  const held = useRef<HTMLElement | null>(null);

  useLayoutEffect(() => {
    const was = before.current;
    before.current = { step, busy };
    const body = bodyRef.current;
    const panel = body?.closest<HTMLElement>('[role="dialog"]') ?? null;
    if (body === null || panel === null) return;
    const active = document.activeElement;
    const inPanel = active instanceof HTMLElement && panel.contains(active);
    if (busy) {
      if (!was.busy && (!inPanel || active.matches(':disabled'))) panel.focus();
      return;
    }
    const inBody = active instanceof HTMLElement && body.contains(active);
    const parked = !inPanel || active === panel || active.matches(':disabled');
    if (was.step !== step ? !inBody : was.busy && parked) (firstStop(body) ?? panel).focus();
  }, [step, busy]);

  // After every render: the control that had the focus is gone, and the focus fell out of the
  // dialog (a browser drops it onto the page). Not while another dialog covers this one.
  useLayoutEffect(() => {
    const was = held.current;
    if (was === null || was.isConnected) return;
    held.current = null;
    const active = document.activeElement;
    if (active instanceof HTMLElement && active !== document.body && active.isConnected) return;
    const body = bodyRef.current;
    const panel = body?.closest<HTMLElement>('[role="dialog"]') ?? null;
    if (body === null || panel === null || panel.closest('[inert]') !== null) return;
    (firstStop(body) ?? panel).focus();
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!nextDisabled && !busy) onNext();
  };
  const current = steps?.[step];

  return (
    <ModalFrame
      labelledBy={`${id}-title`}
      width={width}
      layer="dialog"
      onClose={onClose}
      closable={!busy}
    >
      <form
        className="wizard"
        onSubmit={submit}
        onKeyDown={(event) => {
          if (event.key === 'Enter' && event.repeat) event.preventDefault();
        }}
        onFocus={(event) => {
          held.current = event.target;
        }}
        aria-busy={busy || undefined}
      >
        <div className="modal-head wizard-head">
          <h2 className="modal-title wizard-title" id={`${id}-title`}>
            {title}
          </h2>
          {steps !== undefined && (
            <ol className="stepper" aria-label="Steps">
              {steps.map((label, index) => {
                const state = index < step ? 'done' : index === step ? 'current' : 'todo';
                return (
                  <li
                    key={label}
                    className="stepper-step"
                    data-state={state}
                    aria-current={state === 'current' ? 'step' : undefined}
                  >
                    <span className="stepper-mark" aria-hidden="true">
                      {state === 'done' ? <CheckIcon /> : index + 1}
                    </span>
                    {label}
                    {state === 'done' && <span className="sr-only"> (done)</span>}
                  </li>
                );
              })}
            </ol>
          )}
          <IconButton label="Close" icon={XIcon} onClick={onClose} disabled={busy} />
        </div>
        {steps !== undefined && current !== undefined && (
          <p className="sr-only" role="status" aria-live="polite">
            {`Step ${step + 1} of ${steps.length}: ${current}`}
          </p>
        )}
        {/* Keyed by the step: each step is a new body. Unkeyed, React would carry a control over
            to the next step when the same element sits in the same place, focus and all. */}
        <div key={step} ref={bodyRef} className="modal-body wizard-body" data-modal-body>
          {children}
        </div>
        <div className="modal-foot wizard-foot">
          <span className="wizard-hint">{hint}</span>
          {onBack !== undefined && step > 0 && (
            <Button size={32} onClick={onBack} disabled={busy}>
              Back
            </Button>
          )}
          <Button
            size={32}
            type="submit"
            variant="primary"
            disabled={nextDisabled || busy}
            onClick={(event) => {
              // A submit button's click submits: its default is cancelled for a repeated click.
              if (event.detail > 1) event.preventDefault();
            }}
          >
            {nextLabel}
          </Button>
        </div>
      </form>
    </ModalFrame>
  );
}
