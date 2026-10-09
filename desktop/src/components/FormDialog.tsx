// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The design's openForm (brief §3), with its three corrections: `required` counts visible
// fields only and hidden values stay out of the submission; the dialog stays open while
// onSubmit runs and shows a rejection inline; a backdrop click never dismisses it.
import { type FormEvent, useId, useState } from 'react';
import { uiErrorOf } from '../ipc/api';
import type { UiError } from '../ipc/generated/UiError';
import { Button } from './Button';
import { ErrorPanel } from './ErrorPanel';
import { Eyebrow } from './Eyebrow';
import { type Icon, PencilSimpleIcon } from './icons';
import { ModalFrame } from './Modal';
import { PasswordField } from './PasswordField';
import { SegmentedControl } from './SegmentedControl';
import { Switch } from './Switch';
import { TextField } from './TextField';

export type FormValue = string | boolean | readonly string[] | undefined;
export type FormValues = Readonly<Record<string, FormValue>>;

/** An option: its value, or [value, label]. */
export type Opt = string | readonly [value: string, label: string];

export type FormField = {
  readonly key: string;
  readonly label: string;
  readonly hint?: string;
  /** Shown only when this holds; hidden, it is neither required nor submitted. */
  readonly when?: (values: FormValues) => boolean;
} & (
  | {
      readonly kind?: 'text';
      readonly type?: 'text' | 'password';
      readonly def?: string;
      readonly placeholder?: string;
    }
  | { readonly kind: 'seg'; readonly options: readonly Opt[]; readonly def?: string }
  | {
      readonly kind: 'chips';
      readonly options: readonly string[];
      readonly def?: readonly string[];
    }
  | { readonly kind: 'toggle'; readonly toggleLabel: string; readonly def?: boolean }
);

export interface FormSpec {
  title: string;
  icon?: Icon;
  sub?: string;
  fields: readonly FormField[];
  /** Keys that must not be missing — undefined, '' or false (a required toggle must be on). */
  required?: readonly string[];
  /** The submit label; 'Save' by default. */
  submit?: string;
  danger?: boolean;
  /** The dialog closes once this resolves; a rejection is shown inline and it stays open. */
  onSubmit: (values: FormValues) => Promise<void> | void;
}

export interface FormDialogProps extends FormSpec {
  onClose: () => void;
}

function initial(field: FormField): FormValue {
  if (field.def !== undefined) return field.def;
  if (field.kind === 'chips') return [];
  if (field.kind === 'toggle') return false;
  return '';
}

const missing = (value: FormValue) => value === undefined || value === '' || value === false;

const optionOf = (opt: Opt) =>
  typeof opt === 'string' ? { value: opt, label: opt } : { value: opt[0], label: opt[1] };

export function FormDialog({
  title,
  icon: TitleIcon = PencilSimpleIcon,
  sub,
  fields,
  required = [],
  submit = 'Save',
  danger = false,
  onSubmit,
  onClose,
}: FormDialogProps) {
  const id = useId();
  const [values, setValues] = useState<FormValues>(() =>
    Object.fromEntries(fields.map((field) => [field.key, initial(field)])),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<UiError | null>(null);

  const visible = fields.filter((field) => field.when === undefined || field.when(values));
  const blocked = visible.some(
    (field) => required.includes(field.key) && missing(values[field.key]),
  );
  const set = (key: string, value: FormValue) => setValues({ ...values, [key]: value });

  const send = async (event: FormEvent) => {
    event.preventDefault();
    if (blocked || busy) return;
    setBusy(true);
    setError(null);
    const submission = Object.fromEntries(visible.map((field) => [field.key, values[field.key]]));
    try {
      await onSubmit(submission);
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
      width={500}
      layer="form"
      variant="alert"
      onClose={onClose}
      closable={!busy}
    >
      <form className="alert-form" onSubmit={send}>
        <div className="alert-head">
          <TitleIcon className="alert-icon" aria-hidden="true" />
          <div className="alert-titles">
            <h2 className="alert-title" id={`${id}-title`}>
              {title}
            </h2>
            {sub !== undefined && <div className="alert-sub">{sub}</div>}
          </div>
        </div>
        <div className="form-fields" data-modal-body>
          {visible.map((field) => (
            <Field
              key={field.key}
              id={`${id}-${field.key}`}
              field={field}
              value={values[field.key]}
              onChange={(value) => set(field.key, value)}
            />
          ))}
        </div>
        {error !== null && <ErrorPanel error={error} />}
        <div className="alert-foot">
          <Button size={32} onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            size={32}
            type="submit"
            variant={danger ? 'danger-solid' : 'primary'}
            disabled={blocked || busy}
          >
            {submit}
          </Button>
        </div>
      </form>
    </ModalFrame>
  );
}

function Field({
  id,
  field,
  value,
  onChange,
}: {
  id: string;
  field: FormField;
  value: FormValue;
  onChange: (value: FormValue) => void;
}) {
  const hint =
    field.hint === undefined ? null : (
      <div className="field-hint" id={`${id}-hint`}>
        {field.hint}
      </div>
    );
  switch (field.kind) {
    case 'seg':
      return (
        <div className="field">
          <Eyebrow id={`${id}-label`}>{field.label}</Eyebrow>
          <SegmentedControl
            labelledBy={`${id}-label`}
            size={26}
            font="mono"
            value={typeof value === 'string' ? value : ''}
            options={field.options.map(optionOf)}
            onChange={onChange}
          />
          {hint}
        </div>
      );
    case 'chips': {
      const chosen = Array.isArray(value) ? (value as readonly string[]) : [];
      return (
        <div className="field">
          <Eyebrow id={`${id}-label`}>{field.label}</Eyebrow>
          <fieldset className="chips" aria-labelledby={`${id}-label`}>
            {field.options.map((option) => {
              const on = chosen.includes(option);
              return (
                <button
                  key={option}
                  type="button"
                  className="chip"
                  aria-pressed={on}
                  onClick={() =>
                    onChange(on ? chosen.filter((c) => c !== option) : [...chosen, option])
                  }
                >
                  {option}
                </button>
              );
            })}
          </fieldset>
          {hint}
        </div>
      );
    }
    case 'toggle':
      return (
        <div className="field">
          <Eyebrow>{field.label}</Eyebrow>
          <div className="form-toggle">
            <Switch
              id={id}
              label={field.toggleLabel}
              checked={value === true}
              onChange={(next) => onChange(next)}
            />
            <label htmlFor={id}>{field.toggleLabel}</label>
          </div>
          {hint}
        </div>
      );
    default: {
      const text = {
        label: field.label,
        value: typeof value === 'string' ? value : '',
        onChange: (next: string) => onChange(next),
        ...(field.placeholder !== undefined && { placeholder: field.placeholder }),
        ...(field.hint !== undefined && { hint: field.hint }),
      };
      return field.type === 'password' ? <PasswordField {...text} /> : <TextField {...text} />;
    }
  }
}
