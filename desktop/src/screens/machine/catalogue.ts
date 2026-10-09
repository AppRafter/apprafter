// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The machine picker's decisions, kept apart from its markup: which regions, in what order, which
// offers a filter shows, what a row's note says, the prices and the summary line. Region-first
// (the design): latency is a region's, so it orders the region chips; inside one region's table
// it would be one value in every row (spec §7). What counts as choosable is the core's SKU check
// (cli-providers `classify`): a retired type is refused before a sold-out one, and a type that is
// only announced to retire can still be chosen.
import type { ChipSelectOption } from '../../components/ChipSelect';
import type { MachineCatalogue } from '../../ipc/generated/MachineCatalogue';
import type { MachineOfferView } from '../../ipc/generated/MachineOfferView';
import type { RegionLatency } from '../../ipc/generated/RegionLatency';

/** The region the CLI checks a SKU against when none is set (`execute_add`, `target machine`). */
export const CLI_DEFAULT_REGION = 'nbg1';

export type ArchFilter = 'all' | 'x86' | 'arm';
export type CpuFilter = 'all' | 'shared' | 'dedicated';
export interface OfferFilter {
  readonly arch: ArchFilter;
  readonly cpu: CpuFilter;
  readonly hideUnavailable: boolean;
}
export interface OfferNote {
  readonly text: string;
  readonly tone: 'accent' | 'warn' | 'faint';
}

/** Whether the core's SKU check would take this offer; no offer is never choosable. */
export const choosable = (o: MachineOfferView | undefined): boolean =>
  o?.available === true && !o.retired;

/** `undefined`: not measured yet; `null`: the probe got no answer; else milliseconds. */
type Measured = number | null | undefined;

const rank = (ms: Measured) => (typeof ms === 'number' ? ms : Number.POSITIVE_INFINITY);

function latencyText(ms: Measured, covered: boolean): string {
  if (ms === undefined) return covered ? '–' : '…';
  return ms === null ? 'no answer' : `${ms} ms`;
}

/**
 * The region chips: regions with at least one offer, nearest first once `latencies` is in (a
 * region with no answer, or one the probes did not cover, after the measured ones), then by code.
 * `latencies` is null while the probes run.
 */
export function regionChips(
  cat: MachineCatalogue,
  latencies: readonly RegionLatency[] | null,
): ChipSelectOption<string>[] {
  const offered = new Set(cat.offers.map((o) => o.location));
  const measured = new Map(latencies?.map((l) => [l.region, l.latencyMs]) ?? []);
  return cat.regions
    .filter((r) => offered.has(r.code))
    .map((r) => ({ r, ms: measured.get(r.code) }))
    .sort((a, b) => rank(a.ms) - rank(b.ms) || a.r.code.localeCompare(b.r.code))
    .map(({ r, ms }) => ({
      value: r.code,
      label: r.code,
      secondary: r.city,
      meta: latencyText(ms, latencies !== null),
    }));
}

/** One region's offers under the filters; `hidden` counts what "Hide unavailable" left out. */
export function visibleOffers(
  cat: MachineCatalogue,
  region: string,
  f: OfferFilter,
): { rows: MachineOfferView[]; hidden: number } {
  const inRegion = cat.offers.filter(
    (o) =>
      o.location === region &&
      (f.arch === 'all' || o.arch === f.arch) &&
      (f.cpu === 'all' || o.cpuType === f.cpu),
  );
  if (!f.hideUnavailable) return { rows: inRegion, hidden: 0 };
  const rows = inRegion.filter(choosable);
  return { rows, hidden: inRegion.length - rows.length };
}

/** A row's note, in the order the core's check refuses: retired, then sold out. */
export function offerNote(o: MachineOfferView): OfferNote | null {
  if (o.retired) return { text: 'retired', tone: 'faint' };
  if (!o.available) return { text: 'sold out here', tone: 'faint' };
  if (o.deprecation !== null) {
    const after = o.deprecation.unavailableAfter;
    return {
      text: after === null ? 'retiring' : `retiring after ${after.slice(0, 10)}`,
      tone: 'warn',
    };
  }
  return o.recommended ? { text: 'recommended', tone: 'accent' } : null;
}

export function defaultRegion(cat: MachineCatalogue): string | null {
  const offered = new Set(cat.offers.map((o) => o.location));
  if (offered.has(CLI_DEFAULT_REGION)) return CLI_DEFAULT_REGION;
  return cat.regions.find((r) => offered.has(r.code))?.code ?? null;
}

export function defaultSku(cat: MachineCatalogue, region: string): string | null {
  return (
    cat.offers.find((o) => o.location === region && o.recommended && choosable(o))?.sku ?? null
  );
}

export function offerIn(
  cat: MachineCatalogue,
  region: string,
  sku: string | null,
): MachineOfferView | undefined {
  return sku === null ? undefined : cat.offers.find((o) => o.location === region && o.sku === sku);
}

/** The provider's net price as a number; none or an unreadable one is null. */
export function priceValue(net: string | null): number | null {
  if (net === null) return null;
  const n = Number(net);
  return Number.isFinite(n) ? n : null;
}

/** A price cell: the amount only (the column header carries the currency and the period). */
export function formatAmount(net: string | null, digits: 2 | 4): string {
  const n = priceValue(net);
  return n === null ? '–' : n.toFixed(digits);
}

export function formatPrice(net: string | null, digits: 2 | 4): string {
  const amount = formatAmount(net, digits);
  return amount === '–' ? amount : `€${amount}`;
}

export const formatGb = (gb: number): string => (Number.isInteger(gb) ? String(gb) : gb.toFixed(1));

/** The line under the table: the chosen offer as one server, or why it cannot be had. */
export function machineSummary(o: MachineOfferView | undefined, region: string): string {
  if (o === undefined) return 'Pick a machine.';
  if (o.retired) return `${o.sku} is retired: pick another machine.`;
  if (!o.available) return `${o.sku} is sold out in ${region}: pick another machine.`;
  const monthly =
    priceValue(o.priceMonthlyNet) === null
      ? 'price not listed'
      : `${formatPrice(o.priceMonthlyNet, 2)} / mo, excl. VAT`;
  return `${o.sku} · ${o.cores} vCPU · ${formatGb(o.memoryGb)} GB RAM · ${o.diskGb} GB SSD · ${monthly} · one server`;
}
