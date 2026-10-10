// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { useId, useState } from 'react';
import { type ErrorAction, errorAction } from '../ipc/errors';
import type { UiError } from '../ipc/generated/UiError';
import { Button } from './Button';
import { CaretRightIcon, WarningCircleIcon } from './icons';

const ACTION_LABELS: Record<Exclude<ErrorAction['kind'], 'none'>, string> = {
  'add-target': 'Add a target',
  'renew-token': 'Renew token',
  toolchain: 'Show the toolchain',
  'machine-picker': 'Pick another machine',
  import: 'Rebuild from provider',
  'backup-status': 'Open backup status',
  'running-op': 'Show the running operation',
};

export interface ErrorPanelProps {
  error: UiError;
  /** Offers the action spec §5.1 maps the code to; without a handler nothing is offered. */
  onAction?: (action: ErrorAction) => void;
}

/** A UiError as it is: the message, its code, the help, the causes on request, its action. */
export function ErrorPanel({ error, onAction }: ErrorPanelProps) {
  const [open, setOpen] = useState(false);
  const causesId = useId();
  const action = errorAction(error);
  const count = error.causes.length;
  return (
    <div className="error-panel" role="alert">
      <WarningCircleIcon className="error-icon" aria-hidden="true" />
      <div className="error-main">
        <div className="error-message">{error.message}</div>
        {error.code !== null && <div className="error-code">{error.code}</div>}
        {error.help !== null && <div className="error-help">{error.help}</div>}
        {count > 0 && (
          <>
            <button
              type="button"
              className="error-causes-toggle"
              aria-expanded={open}
              aria-controls={causesId}
              onClick={() => setOpen(!open)}
            >
              <CaretRightIcon className="error-caret" aria-hidden="true" />
              {`${open ? 'Hide' : 'Show'} ${count} ${count === 1 ? 'cause' : 'causes'}`}
            </button>
            {open && (
              <ul className="error-causes" id={causesId}>
                {error.causes.map((cause, index) => (
                  // Causes are a fixed list, read once: the index is a stable key.
                  // biome-ignore lint/suspicious/noArrayIndexKey: see above
                  <li key={index}>{cause}</li>
                ))}
              </ul>
            )}
          </>
        )}
        {action.kind !== 'none' && onAction !== undefined && (
          <div className="error-actions">
            <Button size={28} onClick={() => onAction(action)}>
              {ACTION_LABELS[action.kind]}
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}
