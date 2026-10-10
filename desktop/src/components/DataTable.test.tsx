// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { type DataColumn, DataTable, nextSort, type SortState, sortRows } from './DataTable';

interface Row {
  sku: string;
  price: number | null;
  ok: boolean;
}
const ROWS: Row[] = [
  { sku: 'b', price: 7, ok: true },
  { sku: 'a', price: null, ok: true },
  { sku: 'c', price: 3, ok: false },
  { sku: 'd', price: 7, ok: true },
];
const COLUMNS: DataColumn<Row>[] = [
  { id: 'sku', header: 'Type', sortValue: (r) => r.sku, cell: (r) => r.sku },
  {
    id: 'price',
    header: '€ / mo',
    align: 'end',
    sortValue: (r) => r.price,
    cell: (r) => String(r.price),
  },
  { id: 'note', header: 'Note', headerHidden: true, cell: () => '' },
];
const sort = (column: string, direction: SortState['direction']): SortState => ({
  column,
  direction,
});

describe('sortRows', () => {
  test('numbers ascend and descend; a null sorts last either way; ties keep their order', () => {
    expect(sortRows(ROWS, COLUMNS, sort('price', 'ascending')).map((r) => r.sku)).toEqual([
      'c',
      'b',
      'd',
      'a',
    ]);
    expect(sortRows(ROWS, COLUMNS, sort('price', 'descending')).map((r) => r.sku)).toEqual([
      'b',
      'd',
      'c',
      'a',
    ]);
  });
  test('strings compare naturally (cx22 before cx112)', () => {
    const rows = [
      { sku: 'cx112', price: 1, ok: true },
      { sku: 'cx22', price: 1, ok: true },
    ];
    expect(sortRows(rows, COLUMNS, sort('sku', 'ascending')).map((r) => r.sku)).toEqual([
      'cx22',
      'cx112',
    ]);
  });
  test('a column that does not sort leaves the order as given', () => {
    expect(sortRows(ROWS, COLUMNS, sort('note', 'ascending')).map((r) => r.sku)).toEqual([
      'b',
      'a',
      'c',
      'd',
    ]);
  });
  test('nextSort: the same column flips, another starts ascending', () => {
    expect(nextSort(sort('price', 'ascending'), 'price')).toEqual(sort('price', 'descending'));
    expect(nextSort(sort('price', 'descending'), 'sku')).toEqual(sort('sku', 'ascending'));
  });
});

describe('DataTable', () => {
  const table = (more: Partial<Parameters<typeof DataTable<Row>>[0]> = {}) =>
    render(
      <DataTable<Row>
        label="Machines in nbg1"
        columns={COLUMNS}
        rows={ROWS}
        rowKey={(r) => r.sku}
        sort={sort('price', 'ascending')}
        onSort={() => {}}
        empty="Nothing matches."
        {...more}
      />,
    );

  test('a named table in sorted order; aria-sort only on sortable headers', () => {
    table();
    expect(screen.getByRole('table', { name: 'Machines in nbg1' })).toBeDefined();
    const price = screen.getByRole('columnheader', { name: /€ \/ mo/ });
    expect(price.getAttribute('aria-sort')).toBe('ascending');
    expect(screen.getByRole('columnheader', { name: 'Type' }).getAttribute('aria-sort')).toBe(
      'none',
    );
    expect(screen.getByRole('columnheader', { name: 'Note' }).hasAttribute('aria-sort')).toBe(
      false,
    );
    const bodyRows = screen.getAllByRole('row').slice(1);
    expect(bodyRows.map((r) => within(r).getAllByRole('cell')[0]?.textContent)).toEqual([
      'c',
      'b',
      'd',
      'a',
    ]);
  });

  test('a header click asks for the next sort', async () => {
    const onSort = mock();
    table({ onSort });
    await userEvent
      .setup()
      .click(within(screen.getByRole('columnheader', { name: /€ \/ mo/ })).getByRole('button'));
    expect(onSort).toHaveBeenCalledWith(sort('price', 'descending'));
  });

  test('a sort header is a button the keyboard reaches; Enter sorts by it', async () => {
    const onSort = mock();
    table({ onSort });
    const user = userEvent.setup();
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Type' }));
    await user.keyboard('{Enter}');
    expect(onSort).toHaveBeenCalledWith(sort('sku', 'ascending'));
  });

  test('chosen rows: a radio per row, a row click chooses, a disabled row cannot be chosen', async () => {
    const onChoose = mock();
    table({
      choose: { selected: 'b', onChoose, disabled: (r) => !r.ok, radioLabel: (r) => r.sku },
    });
    expect((screen.getByRole('radio', { name: 'b' }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole('radio', { name: 'c' }) as HTMLInputElement).disabled).toBe(true);
    const user = userEvent.setup();
    await user.click(screen.getByText('d'));
    await user.click(screen.getByText('c'));
    expect(onChoose.mock.calls).toEqual([['d']]);
  });

  test('a row click puts the focus on its radio, so the arrow keys go on from there', async () => {
    table({ choose: { selected: 'b', onChoose: () => {}, radioLabel: (r) => r.sku } });
    await userEvent.setup().click(screen.getByText('d'));
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'd' }));
  });

  test('a click on the radio itself chooses once', async () => {
    const onChoose = mock();
    table({ choose: { selected: 'b', onChoose, radioLabel: (r) => r.sku } });
    await userEvent.setup().click(screen.getByRole('radio', { name: 'd' }));
    expect(onChoose.mock.calls).toEqual([['d']]);
  });

  test('the radios are one group, with a header cell that names the column', () => {
    table({ choose: { selected: null, onChoose: () => {}, radioLabel: (r) => r.sku } });
    const names = new Set(screen.getAllByRole('radio').map((r) => (r as HTMLInputElement).name));
    expect(names.size).toBe(1);
    expect(screen.getByRole('columnheader', { name: 'Chosen' })).toBeDefined();
  });

  test('Home and End choose the first and last row that can be chosen, in the order shown', async () => {
    const onChoose = mock();
    function Harness() {
      const [selected, setSelected] = useState<string | null>('d');
      return (
        <DataTable<Row>
          label="Machines in nbg1"
          columns={COLUMNS}
          rows={ROWS}
          rowKey={(r) => r.sku}
          sort={sort('price', 'ascending')}
          onSort={() => {}}
          empty="Nothing matches."
          choose={{
            selected,
            onChoose: (key) => {
              onChoose(key);
              setSelected(key);
            },
            disabled: (r) => !r.ok,
            radioLabel: (r) => r.sku,
          }}
        />
      );
    }
    render(<Harness />);
    const user = userEvent.setup();
    act(() => screen.getByRole('radio', { name: 'd' }).focus());
    await user.keyboard('{End}');
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'a' }));
    await user.keyboard('{Home}');
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: 'b' }));
    expect(onChoose.mock.calls).toEqual([['a'], ['b']]);
  });

  test('without choose, rows have no radio and the table is not marked as one to choose in', () => {
    table();
    expect(screen.queryAllByRole('radio')).toEqual([]);
    expect(screen.getByRole('table').hasAttribute('data-choose')).toBe(false);
  });

  test('no rows: the empty text in one cell across the table; `after` renders below', () => {
    table({ rows: [], after: <button type="button">Show 2 unavailable</button> });
    expect(screen.getByText('Nothing matches.').closest('td')?.getAttribute('colspan')).toBe('3');
    expect(screen.getByRole('button', { name: 'Show 2 unavailable' })).toBeDefined();
  });

  test('with choose, the empty cell spans the radio column too', () => {
    table({ rows: [], choose: { selected: null, onChoose: () => {}, radioLabel: (r) => r.sku } });
    expect(screen.getByText('Nothing matches.').closest('td')?.getAttribute('colspan')).toBe('4');
  });
});
