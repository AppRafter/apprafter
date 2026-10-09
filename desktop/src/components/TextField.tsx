// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { type InputHTMLAttributes, type ReactNode, type Ref, useId } from 'react';
import { Eyebrow } from './Eyebrow';

export interface TextFieldProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, 'value' | 'onChange' | 'type' | 'id'> {
  label: string;
  value: string;
  onChange: (value: string) => void;
  type?: 'text' | 'password';
  hint?: ReactNode;
  /** Roboto Mono, as form fields are; sans for the lock screen. */
  mono?: boolean;
  /** `bg` inside an overlay, `surface` on the lock screen and filters. */
  background?: 'bg' | 'surface';
  /** Inside the box, at the right (the password field's reveal button). */
  trailing?: ReactNode;
  /** The input element, to move the focus to it. */
  ref?: Ref<HTMLInputElement>;
}

/** An Eyebrow label, a 34px input and a hint, wired together for a screen reader. */
export function TextField({
  label,
  value,
  onChange,
  type = 'text',
  hint,
  mono = true,
  background = 'bg',
  trailing,
  ...rest
}: TextFieldProps) {
  const id = useId();
  const hintId = `${id}-hint`;
  return (
    <div className="field">
      <Eyebrow as="label" htmlFor={id}>
        {label}
      </Eyebrow>
      <div className="field-box">
        <input
          autoComplete="off"
          spellCheck={false}
          {...rest}
          id={id}
          className="input"
          type={type}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          aria-describedby={hint === undefined ? undefined : hintId}
          data-mono={mono || undefined}
          data-background={background}
          data-trailing={trailing === undefined ? undefined : true}
        />
        {trailing}
      </div>
      {hint !== undefined && (
        <div className="field-hint" id={hintId}>
          {hint}
        </div>
      )}
    </div>
  );
}
