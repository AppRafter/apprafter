// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One choice of a few, as cards (the wizard's provider and tier). Native radios in a fieldset: the
// arrow keys, the group's name and the skipping of a disabled card come from the engine, the same
// in WebKitGTK, WKWebView and WebView2; Home and End are radioKeys'. The radio is hidden; the card
// is its label and shows the focus ring while the radio has the keyboard (components.css).
import { type ReactNode, useId } from 'react';
import { onRadioHomeEnd } from './radioKeys';

export interface ChoiceCardOption<V extends string> {
  readonly value: V;
  readonly label: string;
  readonly sub?: string;
  readonly disabled?: boolean;
}

export interface ChoiceCardGroupProps<V extends string> {
  readonly legend: string;
  readonly value: V | null;
  readonly options: readonly ChoiceCardOption<V>[];
  readonly onChange: (value: V) => void;
  /** Below the cards; it also describes the group to a screen reader. */
  readonly hint?: ReactNode;
  readonly columns?: 2 | 3 | 4;
}

export function ChoiceCardGroup<V extends string>({
  legend,
  value,
  options,
  onChange,
  hint,
  columns = 3,
}: ChoiceCardGroupProps<V>) {
  const id = useId();
  const hintId = `${id}-hint`;
  return (
    <fieldset
      className="choice-cards"
      aria-describedby={hint === undefined ? undefined : hintId}
      onKeyDown={onRadioHomeEnd}
    >
      <legend className="eyebrow">{legend}</legend>
      <div className="choice-cards-grid" data-columns={columns}>
        {options.map((option) => (
          <label
            key={option.value}
            className="choice-card"
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
            <span className="choice-card-label">{option.label}</span>
            {option.sub !== undefined && <span className="choice-card-sub">{option.sub}</span>}
          </label>
        ))}
      </div>
      {hint !== undefined && (
        <div className="field-hint choice-cards-hint" id={hintId}>
          {hint}
        </div>
      )}
    </fieldset>
  );
}
