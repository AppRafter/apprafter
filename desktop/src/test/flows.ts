// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// D.3 reports as the tests start from them, the fields a test cares about overridden. Typed by
// the generated types, so a field D.3a renames fails `tsc` here.
import type { MachineCatalogue } from '../ipc/generated/MachineCatalogue';
import type { MachineOfferView } from '../ipc/generated/MachineOfferView';

export function offer(more: Partial<MachineOfferView> = {}): MachineOfferView {
  return {
    location: 'nbg1',
    sku: 'cx22',
    cores: 2,
    memoryGb: 4,
    diskGb: 40,
    arch: 'x86',
    cpuType: 'shared',
    priceMonthlyNet: '3.7900000000',
    priceHourlyNet: '0.0060000000',
    available: true,
    recommended: false,
    deprecation: null,
    retired: false,
    ...more,
  };
}

/**
 * Four regions in the core's order (by code), three with offers (sin has none); nbg1 holds every
 * kind of row.
 */
export function catalogue(): MachineCatalogue {
  return {
    regions: [
      { code: 'fsn1', city: 'Falkenstein', country: 'DE', description: 'Falkenstein DC Park 1' },
      { code: 'hel1', city: 'Helsinki', country: 'FI', description: 'Helsinki DC Park 1' },
      { code: 'nbg1', city: 'Nuremberg', country: 'DE', description: 'Nuremberg DC Park 1' },
      { code: 'sin', city: 'Singapore', country: 'SG', description: 'Singapore' },
    ],
    offers: [
      offer({ sku: 'cx22', recommended: true }),
      offer({
        sku: 'cpx22',
        cores: 3,
        memoryGb: 4,
        diskGb: 80,
        priceMonthlyNet: '7.5500000000',
        priceHourlyNet: '0.0121000000',
      }),
      offer({ sku: 'cax11', arch: 'arm', priceMonthlyNet: '3.7900000000' }),
      offer({
        sku: 'ccx13',
        cpuType: 'dedicated',
        cores: 2,
        memoryGb: 8,
        diskGb: 80,
        priceMonthlyNet: null,
        priceHourlyNet: null,
      }),
      offer({ sku: 'cx32', cores: 4, memoryGb: 8, diskGb: 80, available: false }),
      offer({
        sku: 'cx11',
        retired: true,
        available: false,
        deprecation: {
          announced: '2025-06-01T00:00:00+00:00',
          unavailableAfter: '2025-09-01T00:00:00+00:00',
        },
      }),
      offer({
        sku: 'cpx11',
        deprecation: {
          announced: '2026-08-01T00:00:00+00:00',
          unavailableAfter: '2026-12-01T00:00:00+00:00',
        },
      }),
      offer({ location: 'hel1', sku: 'cx22' }),
      offer({ location: 'fsn1', sku: 'cpx22' }),
    ],
  };
}
