// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One choice of many, as chips with a secondary text and a meta (the machine picker's regions:
// code, city, latency). Native radios, one name, so the engine's arrow keys move the choice and
// skip a disabled chip; Home and End are radioKeys'. The design sets the group's label inline,
// at the start of the chips' row; a fieldset's legend cannot sit there in every engine, so the
// group is a role="group" named by that visible label.
import { useId } from 'react';
import { onRadioHomeEnd } from './radioKeys';

export interface ChipSelectOption<V extends string> {
  readonly value: V;
  readonly label: string;
  readonly secondary?: string;
  readonly meta?: string;
  readonly disabled?: boolean;
}

export interface ChipSelectProps<V extends string> {
  readonly legend: string;
  readonly value: V | null;
  readonly options: readonly ChipSelectOption<V>[];
  readonly onChange: (value: V) => void;
}

export function ChipSelect<V extends string>({
  legend,
  value,
  options,
  onChange,
}: ChipSelectProps<V>) {
  const id = useId();
  return (
    // biome-ignore lint/a11y/useSemanticElements: a legend cannot sit inline in the chips' row in every engine; the group is named by the visible label
    <div
      role="group"
      aria-labelledby={`${id}-legend`}
      className="chip-select"
      onKeyDown={onRadioHomeEnd}
    >
      <span className="eyebrow chip-select-legend" id={`${id}-legend`}>
        {legend}
      </span>
      {options.map((option) => (
        <label
          key={option.value}
          className="chip-option"
          data-disabled={option.disabled || undefined}
        >
          <input
            type="radio"
            className="sr-only"
            name={id}
            value={option.value}
            checked={option.value === value}
            disabled={option.disabled ?? false}
            onChange={() => onChange(option.value)}
          />
          <span className="chip-option-label">{option.label}</span>
          {option.secondary !== undefined && (
            <span className="chip-option-secondary">{option.secondary}</span>
          )}
          {option.meta !== undefined && <span className="chip-option-meta">{option.meta}</span>}
        </label>
      ))}
    </div>
  );
}
