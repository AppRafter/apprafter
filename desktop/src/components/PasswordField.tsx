// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { useState } from 'react';
import { IconButton } from './IconButton';
import { EyeIcon, EyeSlashIcon } from './icons';
import { TextField, type TextFieldProps } from './TextField';

export type PasswordFieldProps = Omit<TextFieldProps, 'type' | 'trailing'>;

/** A TextField whose value stays hidden until its reveal button is pressed. */
export function PasswordField(props: PasswordFieldProps) {
  const [shown, setShown] = useState(false);
  return (
    <TextField
      {...props}
      type={shown ? 'text' : 'password'}
      trailing={
        <span className="field-trailing">
          <IconButton
            label={`${shown ? 'Hide' : 'Show'} ${props.label}`}
            icon={shown ? EyeSlashIcon : EyeIcon}
            pressed={shown}
            onClick={() => setShown(!shown)}
          />
        </span>
      }
    />
  );
}
