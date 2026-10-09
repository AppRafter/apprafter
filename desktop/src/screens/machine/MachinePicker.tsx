// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The machine picker (spec §7, target machine): the design's region-first table with its arch and
// CPU-type filters, "Hide unavailable" and sortable prices, plus what the CLI matrix has and the
// design lacked: per-region latency ordering the regions, the retiring flag, the sold-out reveal.
// No Traffic column and no RAM minimum: the catalogue carries neither. "Hide unavailable" starts
// on, as the CLI keeps sold-out rows behind an entry (plan deviation 4).
import { useRef, useState } from 'react';
import { Button } from '../../components/Button';
import { ChipSelect } from '../../components/ChipSelect';
import { type DataColumn, DataTable, type SortState } from '../../components/DataTable';
import { CheckSquareIcon, SquareIcon } from '../../components/icons';
import { SegmentedControl } from '../../components/SegmentedControl';
import type { MachineCatalogue } from '../../ipc/generated/MachineCatalogue';
import type { MachineOfferView } from '../../ipc/generated/MachineOfferView';
import type { RegionLatency } from '../../ipc/generated/RegionLatency';
import {
  type ArchFilter,
  type CpuFilter,
  choosable,
  formatAmount,
  formatGb,
  machineSummary,
  type OfferFilter,
  offerIn,
  offerNote,
  priceValue,
  regionChips,
  visibleOffers,
} from './catalogue';

/** Why the regions show no latency (the reading failed or was cancelled), and how to retry. */
export interface LatencyProblem {
  readonly text: string;
  readonly onRetry: () => void;
}

export interface MachinePickerProps {
  readonly catalogue: MachineCatalogue;
  /** null while the probes run. */
  readonly latencies: readonly RegionLatency[] | null;
  /** The latency could not be measured: said under the regions, which then show none. */
  readonly latencyProblem?: LatencyProblem | null;
  readonly region: string;
  readonly sku: string | null;
  readonly onRegion: (region: string) => void;
  readonly onSku: (sku: string) => void;
}

const ARCH = [
  { value: 'all', label: 'All' },
  { value: 'x86', label: 'x86' },
  { value: 'arm', label: 'Arm' },
] as const;
const CPU = [
  { value: 'all', label: 'Any CPU' },
  { value: 'shared', label: 'Shared' },
  { value: 'dedicated', label: 'Dedicated' },
] as const;

function Note({ offer }: { offer: MachineOfferView }) {
  const note = offerNote(offer);
  return note === null ? null : (
    <span className="machine-note" data-tone={note.tone}>
      {note.text}
    </span>
  );
}

// Widths are the design's columns plus the cells' 8 px of padding.
const COLUMNS: readonly DataColumn<MachineOfferView>[] = [
  {
    id: 'sku',
    header: 'Type',
    width: '72px',
    sortValue: (o) => o.sku,
    cell: (o) => <span className="machine-sku">{o.sku}</span>,
  },
  {
    id: 'cpu',
    header: 'CPU',
    width: '92px',
    sortValue: (o) => o.cpuType,
    cell: (o) => <span className="machine-muted">{o.cpuType}</span>,
  },
  {
    id: 'arch',
    header: 'Arch',
    width: '58px',
    sortValue: (o) => o.arch,
    cell: (o) => <span className="machine-muted">{o.arch === 'arm' ? 'Arm' : o.arch}</span>,
  },
  {
    id: 'cores',
    header: 'vCPU',
    width: '58px',
    align: 'end',
    sortValue: (o) => o.cores,
    cell: (o) => o.cores,
  },
  {
    id: 'memory',
    header: 'RAM',
    width: '68px',
    align: 'end',
    sortValue: (o) => o.memoryGb,
    cell: (o) => `${formatGb(o.memoryGb)} GB`,
  },
  {
    id: 'disk',
    header: 'SSD',
    width: '78px',
    align: 'end',
    sortValue: (o) => o.diskGb,
    cell: (o) => `${o.diskGb} GB`,
  },
  {
    id: 'hourly',
    header: '€ / h',
    width: '72px',
    align: 'end',
    sortValue: (o) => priceValue(o.priceHourlyNet),
    cell: (o) => <span className="machine-muted">{formatAmount(o.priceHourlyNet, 4)}</span>,
  },
  {
    id: 'monthly',
    header: '€ / mo',
    width: '78px',
    align: 'end',
    sortValue: (o) => priceValue(o.priceMonthlyNet),
    cell: (o) => <span className="machine-sku">{formatAmount(o.priceMonthlyNet, 2)}</span>,
  },
  { id: 'note', header: 'Note', headerHidden: true, cell: (o) => <Note offer={o} /> },
];

export function MachinePicker({
  catalogue,
  latencies,
  latencyProblem = null,
  region,
  sku,
  onRegion,
  onSku,
}: MachinePickerProps) {
  const [filter, setFilter] = useState<OfferFilter>({
    arch: 'all',
    cpu: 'all',
    hideUnavailable: true,
  });
  const [sort, setSort] = useState<SortState>({ column: 'monthly', direction: 'ascending' });
  const hideButton = useRef<HTMLButtonElement>(null);
  const { rows, hidden } = visibleOffers(catalogue, region, filter);
  const hide = filter.hideUnavailable;
  const HideIcon = hide ? CheckSquareIcon : SquareIcon;

  // The reveal goes once it is used; the focus goes to the toggle it switched off, never to the
  // page.
  const reveal = () => {
    setFilter({ ...filter, hideUnavailable: false });
    hideButton.current?.focus();
  };

  return (
    <div className="machine-picker">
      <ChipSelect
        legend="Region"
        value={region}
        options={regionChips(catalogue, latencies, latencyProblem === null)}
        onChange={onRegion}
      />
      {latencyProblem !== null && (
        <div className="machine-latency-note" role="note">
          <span>{latencyProblem.text}</span>
          <Button variant="ghost" size={26} onClick={latencyProblem.onRetry}>
            Try again
          </Button>
        </div>
      )}
      <div className="machine-tools">
        <span className="eyebrow machine-tools-legend">Machine</span>
        <span className="machine-caption">{`Live catalogue for ${region} · click a column to sort`}</span>
        <SegmentedControl<ArchFilter>
          ariaLabel="Architecture"
          size={26}
          font="mono"
          value={filter.arch}
          options={ARCH}
          onChange={(arch) => setFilter({ ...filter, arch })}
        />
        <SegmentedControl<CpuFilter>
          ariaLabel="CPU type"
          size={26}
          font="mono"
          value={filter.cpu}
          options={CPU}
          onChange={(cpu) => setFilter({ ...filter, cpu })}
        />
        <button
          ref={hideButton}
          type="button"
          className="machine-hide"
          aria-pressed={hide}
          onClick={() => setFilter({ ...filter, hideUnavailable: !hide })}
        >
          <HideIcon aria-hidden="true" />
          Hide unavailable
        </button>
      </div>
      <DataTable<MachineOfferView>
        label={`Machines in ${region}`}
        columns={COLUMNS}
        rows={rows}
        rowKey={(o) => o.sku}
        sort={sort}
        onSort={setSort}
        choose={{
          selected: sku,
          onChoose: onSku,
          disabled: (o) => !choosable(o),
          radioLabel: (o) => o.sku,
        }}
        maxHeight={292}
        empty="No machine in this region matches these filters."
        after={
          hidden > 0 ? (
            <Button variant="ghost" size={26} onClick={reveal}>
              {`Show ${hidden} unavailable`}
            </Button>
          ) : undefined
        }
      />
      <p className="machine-summary" aria-live="polite">
        {machineSummary(offerIn(catalogue, region, sku), region)}
      </p>
    </div>
  );
}
