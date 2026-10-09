// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// D.3 reports as the tests start from them, the fields a test cares about overridden. Typed by
// the generated types, so a field D.3a renames fails `tsc` here.
import type { Check } from '../ipc/generated/Check';
import type { DoctorReport } from '../ipc/generated/DoctorReport';
import type { MachineCatalogue } from '../ipc/generated/MachineCatalogue';
import type { MachineOfferView } from '../ipc/generated/MachineOfferView';
import type { MachineSet } from '../ipc/generated/MachineSet';
import type { PlanView } from '../ipc/generated/PlanView';
import type { TargetAdded } from '../ipc/generated/TargetAdded';
import type { ToolchainReport } from '../ipc/generated/ToolchainReport';
import type { Verification } from '../ipc/generated/Verification';
import type { WhoamiReport } from '../ipc/generated/WhoamiReport';

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

/**
 * A toolchain probe in the core's tool order, the CLI's purposes and install lines: kubectl found;
 * helm missing; git the macOS stub without the developer tools (it exits 1, its own words the
 * reason); ssh timed out (its macOS line is "preinstalled"); cue found only as a `.cmd` shim.
 */
export function toolchainReport(): ToolchainReport {
  return {
    tools: [
      {
        tool: 'kubectl',
        required: true,
        purpose: 'talking to the cluster',
        path: '/usr/bin/kubectl',
        version: 'Client Version: v1.34.1',
        problem: null,
        install: [
          { os: 'macos', command: 'brew install kubectl' },
          { os: 'debian', command: 'apt install kubectl' },
          { os: 'nix', command: 'nix profile install nixpkgs#kubectl' },
          { os: 'windows', command: 'winget install Kubernetes.kubectl' },
          { os: 'other', command: 'https://kubernetes.io/docs/tasks/tools/' },
        ],
      },
      {
        tool: 'helm',
        required: false,
        purpose: 'installing platform charts',
        path: null,
        version: null,
        problem: { kind: 'not_found' },
        install: [
          { os: 'windows', command: 'winget install Helm.Helm' },
          { os: 'macos', command: 'brew install helm' },
          { os: 'debian', command: 'apt install helm' },
          { os: 'nix', command: 'nix profile install nixpkgs#kubernetes-helm' },
          { os: 'other', command: 'https://helm.sh/docs/intro/install/' },
        ],
      },
      {
        tool: 'git',
        required: false,
        purpose: 'reading the application repository',
        path: '/usr/bin/git',
        version: null,
        problem: {
          kind: 'no_version_output',
          exit: 1,
          detail:
            'xcrun: error: invalid active developer path (/Library/Developer/CommandLineTools)',
        },
        install: [
          { os: 'macos', command: 'xcode-select --install' },
          { os: 'debian', command: 'apt install git' },
          { os: 'nix', command: 'nix profile install nixpkgs#git' },
          { os: 'windows', command: 'winget install Git.Git' },
          { os: 'other', command: 'https://git-scm.com/downloads' },
        ],
      },
      {
        tool: 'ssh',
        required: false,
        purpose: 'reaching the node over SSH',
        path: '/usr/bin/ssh',
        version: null,
        problem: { kind: 'timed_out' },
        install: [
          { os: 'macos', command: 'preinstalled' },
          { os: 'debian', command: 'apt install openssh-client' },
          { os: 'nix', command: 'nix profile install nixpkgs#openssh' },
          {
            os: 'windows',
            command: 'built into Windows 10/11: Settings › Optional features › OpenSSH Client',
          },
        ],
      },
      {
        tool: 'cue',
        required: false,
        purpose: 'validating application manifests',
        path: null,
        version: null,
        problem: { kind: 'unsupported', path: 'C:\\tools\\cue.cmd' },
        install: [
          { os: 'macos', command: 'brew install cue' },
          { os: 'arch', command: 'pacman -S cue' },
          { os: 'nix', command: 'nix profile install nixpkgs#cue' },
          { os: 'windows', command: 'winget install CueLang.Cue' },
          { os: 'other', command: 'https://cuelang.org/docs/introduction/installation/' },
        ],
      },
    ],
    searchPath: ['/usr/local/bin', '/usr/bin'],
    searchPathSource: 'login_shell',
  };
}

/** What `target add` answers for `lab`: verified, cx22 checked in nbg1, the CLI default unmoved. */
export function targetAdded(more: Partial<TargetAdded> = {}): TargetAdded {
  return {
    name: 'lab',
    replaced: false,
    isCliDefault: false,
    cliDefault: null,
    token: { status: 'verified', elapsedMs: 182 },
    sku: { status: 'validated', sku: 'cx22', region: 'nbg1', regionWasDefault: false },
    ...more,
  };
}

/**
 * A bounded add plan for `lab` without its op id, which the IPC harness assigns
 * (`Harness.plan` takes `Omit<PlanView, 'opId'>`). Not called `planView`: fixtures.ts has a
 * `planView` that returns a whole `PlanView` for prod-eu, and two builders of one name with
 * different shapes invite the wrong import.
 */
export function planParts(more: Partial<Omit<PlanView, 'opId'>> = {}): Omit<PlanView, 'opId'> {
  return {
    class: 'bounded',
    title: 'Add target lab',
    changes: [],
    target: 'lab',
    expiresAtMs: Date.now() + 600_000,
    ...more,
  };
}

/** What `target machine` answers for staging: cpx22, checked in nbg1. */
export function machineSet(more: Partial<MachineSet> = {}): MachineSet {
  return {
    name: 'staging',
    sku: 'cpx22',
    region: 'nbg1',
    skuCheck: { status: 'validated', sku: 'cpx22', region: 'nbg1', regionWasDefault: false },
    ...more,
  };
}

/** whoami with prod-eu as the CLI default (Hetzner Cloud, nbg1, cx22, a key that exists). */
export function whoamiReport(verification: Verification): WhoamiReport {
  return {
    identity: 'anonymous_self_hosted',
    cliDefault: {
      status: 'found',
      target: {
        name: 'prod-eu',
        provider: 'hetzner-cloud',
        verification,
        region: 'nbg1',
        serverType: 'cx22',
        defaultTier: 'team',
        clusterName: null,
        sshKey: {
          path: '/home/alex/.ssh/id_ed25519.pub',
          display: '~/.ssh/id_ed25519.pub',
          exists: true,
          algo: 'ssh-ed25519',
        },
      },
    },
  };
}
