// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { catalogue } from '../../test/flows';
import { MachinePicker, type MachinePickerProps } from './MachinePicker';

function picker(more: Partial<MachinePickerProps> = {}) {
  const props: MachinePickerProps = {
    catalogue: catalogue(),
    latencies: [
      { region: 'nbg1', latencyMs: 38 },
      { region: 'hel1', latencyMs: 12 },
    ],
    region: 'nbg1',
    sku: 'cx22',
    onRegion: mock(),
    onSku: mock(),
    ...more,
  };
  render(<MachinePicker {...props} />);
  return { user: userEvent.setup(), ...props };
}
const table = () => screen.getByRole('table');
const skus = () =>
  within(table())
    .getAllByRole('radio')
    .map((r) => r.getAttribute('aria-label'));
const cells = (sku: string) => {
  const row = screen.getByRole('radio', { name: sku }).closest('tr');
  if (row === null) throw new Error(`no row for ${sku}`);
  return within(row)
    .getAllByRole('cell')
    .slice(1)
    .map((c) => c.textContent);
};

describe('MachinePicker', () => {
  test('a latency that could not be measured is said, with Try again, and no chip shows one', async () => {
    const onRetry = mock();
    const { user } = picker({
      latencies: null,
      latencyProblem: { text: 'Latency could not be measured: the probe broke', onRetry },
    });
    expect(screen.getByText('Latency could not be measured: the probe broke')).toBeDefined();
    expect(document.querySelectorAll('.chip-option-meta')).toHaveLength(0);
    await user.click(screen.getByRole('button', { name: 'Try again' }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  test("the region's table, cheapest first by the month; unavailable hidden behind a count", () => {
    picker();
    expect(screen.getByRole('table', { name: 'Machines in nbg1' })).toBeDefined();
    expect(skus()).toEqual(['cx22', 'cax11', 'cpx11', 'cpx22', 'ccx13']);
    expect(
      screen.getByRole('button', { name: 'Hide unavailable' }).getAttribute('aria-pressed'),
    ).toBe('true');
    expect(screen.getByRole('button', { name: 'Show 2 unavailable' })).toBeDefined();
  });

  test('the reveal shows sold-out and retired rows, which cannot be chosen', async () => {
    const { user } = picker();
    await user.click(screen.getByRole('button', { name: 'Show 2 unavailable' }));
    expect((screen.getByRole('radio', { name: 'cx32' }) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole('radio', { name: 'cx11' }) as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByText('sold out here')).toBeDefined();
    expect(screen.getByText('retired')).toBeDefined();
    expect(
      screen.getByRole('button', { name: 'Hide unavailable' }).getAttribute('aria-pressed'),
    ).toBe('false');
    expect(screen.queryByRole('button', { name: /^Show \d+ unavailable$/ })).toBeNull();
  });

  test('the reveal goes once used, and hands the focus to the toggle, not to the page', async () => {
    const { user } = picker();
    await user.click(screen.getByRole('button', { name: 'Show 2 unavailable' }));
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Hide unavailable' }));
  });

  test('Hide unavailable toggles the unavailable rows out and back', async () => {
    const { user } = picker();
    const hide = screen.getByRole('button', { name: 'Hide unavailable' });
    await user.click(hide);
    expect(skus()).toContain('cx32');
    await user.click(hide);
    expect(skus()).not.toContain('cx32');
    expect(hide.getAttribute('aria-pressed')).toBe('true');
  });

  test("a row's cells: the provider's CPU type, the arch, sizes and net prices", () => {
    picker();
    expect(cells('cax11')).toEqual([
      'cax11',
      'shared',
      'Arm',
      '2',
      '4 GB',
      '40 GB',
      '0.0060',
      '3.79',
      '',
    ]);
    expect(cells('ccx13').slice(6)).toEqual(['–', '–', '']);
    expect(cells('cx22').at(-1)).toBe('recommended');
  });

  test('the arch filter, a retiring note, the summary line', async () => {
    const { user } = picker();
    expect(screen.getByText('retiring after 2026-12-01')).toBeDefined();
    expect(
      screen.getByText('cx22 · 2 vCPU · 4 GB RAM · 40 GB SSD · €3.79 / mo, excl. VAT · one server'),
    ).toBeDefined();
    await user.click(
      within(screen.getByRole('radiogroup', { name: 'Architecture' })).getByRole('radio', {
        name: 'Arm',
      }),
    );
    expect(skus()).toEqual(['cax11']);
  });

  test('the CPU filter; a filter that matches nothing says so', async () => {
    const { user } = picker();
    const cpu = within(screen.getByRole('radiogroup', { name: 'CPU type' }));
    await user.click(cpu.getByRole('radio', { name: 'Dedicated' }));
    expect(skus()).toEqual(['ccx13']);
    await user.click(
      within(screen.getByRole('radiogroup', { name: 'Architecture' })).getByRole('radio', {
        name: 'Arm',
      }),
    );
    expect(within(table()).queryAllByRole('radio')).toHaveLength(0);
    expect(screen.getByText('No machine in this region matches these filters.')).toBeDefined();
  });

  test('the regions: named, nearest first, each with its distance', () => {
    picker();
    const regions = screen.getByRole('group', { name: 'Region' });
    expect(
      within(regions)
        .getAllByRole('radio')
        .map((r) => (r as HTMLInputElement).value),
    ).toEqual(['hel1', 'nbg1', 'fsn1']);
    expect(within(regions).getByText('12 ms')).toBeDefined();
    expect((within(regions).getByRole('radio', { name: /nbg1/ }) as HTMLInputElement).checked).toBe(
      true,
    );
  });

  test('a chosen offer that is sold out says so under the table', () => {
    picker({ sku: 'cx32' });
    expect(screen.getByText('cx32 is sold out in nbg1: pick another machine.')).toBeDefined();
  });

  test('choosing a row and a region report them', async () => {
    const { user, onSku, onRegion } = picker();
    await user.click(screen.getByRole('radio', { name: 'cpx22' }));
    await user.click(screen.getByText('Helsinki'));
    expect(onSku).toHaveBeenCalledWith('cpx22');
    expect(onRegion).toHaveBeenCalledWith('hel1');
  });
});
