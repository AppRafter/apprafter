// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// D.3 reports as the tests start from them, the fields a test cares about overridden. Typed by
// the generated types, so a field D.3a renames fails `tsc` here.
import type { Check } from '../ipc/generated/Check';
import type { DoctorReport } from '../ipc/generated/DoctorReport';
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

export function check(more: Partial<Check> = {}): Check {
  return {
    id: 'config_readable',
    tool: null,
    status: 'pass',
    title: 'Config file readable',
    detail: null,
    fix: null,
    ...more,
  };
}

/**
 * A run against `target`, in the core's group order and with its row titles: 3 passed, 2
 * warnings, 1 failed, and 1 skipped row that carries its reason in the detail.
 */
export function doctorReport(target = 'prod-eu'): DoctorReport {
  return {
    target,
    groups: [
      {
        id: 'target',
        checks: [
          check({ detail: `~/.config/apprafter/targets/${target}/config.yaml` }),
          check({
            id: 'token_verified',
            status: 'fail',
            title: 'Token verified against provider API',
            detail: 'HTTP 401: Unable to authenticate',
            fix: { kind: 'renew_token', target, why: 'token_rejected' },
          }),
          check({
            id: 'ssh_key',
            status: 'warn',
            title: 'SSH key path configured',
            fix: { kind: 'configure_ssh_key', target },
          }),
        ],
      },
      {
        id: 'cluster',
        checks: [
          check({
            id: 'kubeconfig_cached',
            status: 'skipped',
            title: 'Kubeconfig cached',
            detail: 'no provisioned server',
          }),
        ],
      },
      {
        id: 'this_computer',
        checks: [
          check({
            id: 'tool',
            tool: 'kubectl',
            title: '`kubectl` on PATH',
            detail: 'Client Version: v1.34.1',
          }),
          check({
            id: 'tool',
            tool: 'helm',
            status: 'warn',
            title: '`helm` on PATH',
            fix: { kind: 'install_tool', tool: 'helm' },
          }),
          check({ id: 'dns', title: 'DNS resolves `api.hetzner.cloud`', detail: '443/tcp' }),
        ],
      },
    ],
  };
}
