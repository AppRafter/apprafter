// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// A table that scrolls in its own box under a sticky header (the machine picker; later slices'
// lists). A sortable column's header is a button and carries aria-sort; the table sorts what it is
// given (stable; a null value sorts last in either direction). With `choose`, each row starts with
// a native radio (one group: the engine's arrow keys move the choice and skip disabled rows;
// Home and End are radioKeys'). A click anywhere on an enabled row chooses it too and puts the
// focus on its radio, so the keyboard goes on from the row the pointer chose.
import { type MouseEvent, type ReactNode, useId } from 'react';
import { CaretDownIcon, CaretUpIcon } from './icons';
import { onRadioHomeEnd } from './radioKeys';

export interface DataColumn<R> {
  readonly id: string;
  readonly header: string;
  /** The header names the column for a screen reader only. */
  readonly headerHidden?: boolean;
  readonly align?: 'start' | 'end';
  /** A fixed width (`72px`); none takes what is left. */
  readonly width?: string;
  /** Present: the column sorts by this value. */
  readonly sortValue?: (row: R) => number | string | null;
  readonly cell: (row: R) => ReactNode;
}

export interface SortState {
  readonly column: string;
  readonly direction: 'ascending' | 'descending';
}

export interface DataTableProps<R> {
  readonly label: string;
  readonly columns: readonly DataColumn<R>[];
  readonly rows: readonly R[];
  readonly rowKey: (row: R) => string;
  readonly sort: SortState;
  readonly onSort: (sort: SortState) => void;
  readonly choose?: {
    readonly selected: string | null;
    readonly onChoose: (key: string) => void;
    readonly disabled?: (row: R) => boolean;
    readonly radioLabel: (row: R) => string;
  };
  readonly maxHeight?: number;
  readonly empty: ReactNode;
  /** Below the rows, inside the scroll box (the sold-out reveal). */
  readonly after?: ReactNode;
}

export function nextSort(current: SortState, column: string): SortState {
  if (current.column !== column) return { column, direction: 'ascending' };
  return { column, direction: current.direction === 'ascending' ? 'descending' : 'ascending' };
}

export function sortRows<R>(
  rows: readonly R[],
  columns: readonly DataColumn<R>[],
  sort: SortState,
): R[] {
  const value = columns.find((c) => c.id === sort.column)?.sortValue;
  if (value === undefined) return [...rows];
  const sign = sort.direction === 'ascending' ? 1 : -1;
  return rows
    .map((row, index) => ({ row, index, key: value(row) }))
    .sort((a, b) => {
      if (a.key === null || b.key === null) {
        if (a.key === b.key) return a.index - b.index;
        return a.key === null ? 1 : -1;
      }
      const order =
        typeof a.key === 'number' && typeof b.key === 'number'
          ? a.key - b.key
          : String(a.key).localeCompare(String(b.key), 'en', { numeric: true });
      return order === 0 ? a.index - b.index : sign * order;
    })
    .map((entry) => entry.row);
}

export function DataTable<R>({
  label,
  columns,
  rows,
  rowKey,
  sort,
  onSort,
  choose,
  maxHeight,
  empty,
  after,
}: DataTableProps<R>) {
  const group = useId();
  const sorted = sortRows(rows, columns, sort);
  const span = columns.length + (choose === undefined ? 0 : 1);

  const chooseRow = (event: MouseEvent<HTMLTableRowElement>, key: string) => {
    const radio = event.currentTarget.querySelector<HTMLInputElement>('input[type="radio"]');
    // A click on the radio itself chooses through the radio's own change.
    if (radio === null || event.target === radio) return;
    radio.focus();
    choose?.onChoose(key);
  };

  return (
    <div className="data-table-scroll" style={maxHeight === undefined ? undefined : { maxHeight }}>
      <table
        className="data-table"
        aria-label={label}
        data-choose={choose === undefined ? undefined : ''}
        onKeyDown={choose === undefined ? undefined : onRadioHomeEnd}
      >
        <colgroup>
          {choose !== undefined && <col style={{ width: '32px' }} />}
          {columns.map((c) => (
            <col key={c.id} style={c.width === undefined ? undefined : { width: c.width }} />
          ))}
        </colgroup>
        <thead>
          <tr>
            {choose !== undefined && (
              <th scope="col">
                <span className="sr-only">Chosen</span>
              </th>
            )}
            {columns.map((c) => {
              const on = sort.column === c.id;
              const ariaSort = c.sortValue === undefined ? undefined : on ? sort.direction : 'none';
              return (
                <th key={c.id} scope="col" aria-sort={ariaSort} data-align={c.align ?? 'start'}>
                  {c.sortValue === undefined ? (
                    <span className={c.headerHidden ? 'sr-only' : undefined}>{c.header}</span>
                  ) : (
                    <button
                      type="button"
                      className="th-sort"
                      onClick={() => onSort(nextSort(sort, c.id))}
                    >
                      {c.header}
                      {on &&
                        (sort.direction === 'ascending' ? (
                          <CaretUpIcon aria-hidden="true" />
                        ) : (
                          <CaretDownIcon aria-hidden="true" />
                        ))}
                    </button>
                  )}
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody>
          {sorted.length === 0 && (
            <tr>
              <td colSpan={span} className="data-table-empty">
                {empty}
              </td>
            </tr>
          )}
          {sorted.map((row) => {
            const key = rowKey(row);
            const off = choose?.disabled?.(row) ?? false;
            const chosen = choose?.selected === key;
            return (
              // The row's click is a pointer convenience; the row's radio is the keyboard's way.
              <tr
                key={key}
                data-chosen={chosen || undefined}
                data-disabled={off || undefined}
                onClick={choose === undefined || off ? undefined : (event) => chooseRow(event, key)}
              >
                {choose !== undefined && (
                  <td>
                    <input
                      type="radio"
                      className="radio-dot"
                      name={group}
                      checked={chosen}
                      disabled={off}
                      aria-label={choose.radioLabel(row)}
                      onChange={() => choose.onChoose(key)}
                    />
                  </td>
                )}
                {columns.map((c) => (
                  <td key={c.id} data-align={c.align ?? 'start'}>
                    {c.cell(row)}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
      {after !== undefined && <div className="data-table-after">{after}</div>}
    </div>
  );
}
