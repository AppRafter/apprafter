// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One choice from a list (the wizard's SSH keys). Native radios, one name: the engine's arrow keys
// move the choice and skip a disabled row; Home and End are radioKeys'. A row's detail and meta
// describe its radio. A row's `expanded` content (a path field) shows under it while it is
// chosen, as a sibling of the row's label: a field inside a label would take the label's clicks,
// and a radio's name must not include a field. It follows the chosen radio, so Tab reaches it
// next; choosing does not move the focus into it, or the arrow keys would stop there.
import { type ReactNode, useId } from 'react';
import { onRadioHomeEnd } from './radioKeys';

export interface RadioListItem<V extends string> {
  readonly value: V;
  /** The radio's accessible name (the row's label may be richer). */
  readonly ariaLabel: string;
  readonly label: ReactNode;
  readonly detail?: ReactNode;
  readonly meta?: ReactNode;
  readonly disabled?: boolean;
  readonly expanded?: ReactNode;
}

export interface RadioListProps<V extends string> {
  readonly legend: string;
  readonly value: V | null;
  readonly items: readonly RadioListItem<V>[];
  readonly onChange: (value: V) => void;
}

export function RadioList<V extends string>({ legend, value, items, onChange }: RadioListProps<V>) {
  const id = useId();
  return (
    <fieldset className="radio-list" onKeyDown={onRadioHomeEnd}>
      <legend className="eyebrow">{legend}</legend>
      <div className="radio-list-rows">
        {items.map((item, index) => {
          const on = item.value === value;
          const detailId = `${id}-${index}-detail`;
          const metaId = `${id}-${index}-meta`;
          const describedBy = [
            item.detail === undefined ? null : detailId,
            item.meta === undefined ? null : metaId,
          ]
            .filter((part) => part !== null)
            .join(' ');
          return (
            <div
              key={item.value}
              className="radio-row"
              data-checked={on || undefined}
              data-disabled={item.disabled || undefined}
            >
              <label className="radio-row-main">
                <input
                  type="radio"
                  className="radio-dot"
                  name={id}
                  value={item.value}
                  checked={on}
                  disabled={item.disabled ?? false}
                  aria-label={item.ariaLabel}
                  aria-describedby={describedBy === '' ? undefined : describedBy}
                  onChange={() => onChange(item.value)}
                />
                <span className="radio-row-label">{item.label}</span>
                {item.detail !== undefined && (
                  <span className="radio-row-detail" id={detailId}>
                    {item.detail}
                  </span>
                )}
                {item.meta !== undefined && (
                  <span className="radio-row-meta" id={metaId}>
                    {item.meta}
                  </span>
                )}
              </label>
              {on && item.expanded !== undefined && (
                <div className="radio-row-extra">{item.expanded}</div>
              )}
            </div>
          );
        })}
      </div>
    </fieldset>
  );
}
