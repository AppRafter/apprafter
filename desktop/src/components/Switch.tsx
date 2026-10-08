// SPDX-License-Identifier: FSL-1.1-Apache-2.0

export interface SwitchProps {
  checked: boolean;
  /** Called with the state asked for; the caller decides (a setting may be refused). */
  onChange: (next: boolean) => void;
  /** The accessible name. */
  label: string;
  disabled?: boolean;
}

/** On/off, 34×20: a button with role=switch, so Space and Enter toggle it. */
export function Switch({ checked, onChange, label, disabled = false }: SwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className="switch"
      disabled={disabled}
      onClick={() => onChange(!checked)}
    >
      <span className="switch-knob" aria-hidden="true" />
    </button>
  );
}
