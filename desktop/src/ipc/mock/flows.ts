// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Richer answers for four reads D.3d answers minimally (fixtures.ts) — the machine catalogue, the
// region latencies, Doctor and the toolchain — for `bun run dev:mock` and the Playwright walks,
// and the clipboard (D.3e deviation 5, overview §5 item 3). Built on D.3d's engine and store:
// every rule stays D.3d's (the token format, the x-token 401, the drafts, taken names, the
// catalogue's source check), so no command that carries one is answered here, the catalogue asks
// D.3d's catalogueSourceRefusal first, and Doctor on a target the store does not hold is D.3d's
// answer. whoami stays D.3d's: it already reads the store. Nothing here is token-shaped.
import type { CatalogueSourceArg } from '../generated/CatalogueSourceArg';
import type { Check } from '../generated/Check';
import type { DoctorReport } from '../generated/DoctorReport';
import type { HintOs } from '../generated/HintOs';
import type { MachineCatalogue } from '../generated/MachineCatalogue';
import type { MachineOfferView } from '../generated/MachineOfferView';
import type { OpEvent } from '../generated/OpEvent';
import type { RegionLatency } from '../generated/RegionLatency';
import type { RegionView } from '../generated/RegionView';
import type { JsonValue } from '../generated/serde_json/JsonValue';
import type { TargetReport } from '../generated/TargetReport';
import type { ToolchainReport } from '../generated/ToolchainReport';
import type { ToolId } from '../generated/ToolId';
import type { ToolStatus } from '../generated/ToolStatus';
import { HETZNER_TOKEN_LEN } from '../generated/target';
import type { Handler, MockOps, MockRun } from './ops';
import { catalogueSourceRefusal, type MockStore } from './targets';

/** The mock's 401 by D.3d's rule (a well-formed token starting with `x`): built, never written. */
export const MOCK_REJECTED_TOKEN = 'x'.repeat(HETZNER_TOKEN_LEN);

// The machine catalogue -------------------------------------------------------------------------

const REGIONS: readonly RegionView[] = [
  { code: 'nbg1', city: 'Nuremberg', country: 'DE', description: 'Nuremberg 1 DC 3' },
  { code: 'fsn1', city: 'Falkenstein', country: 'DE', description: 'Falkenstein 1 DC14' },
  { code: 'hel1', city: 'Helsinki', country: 'FI', description: 'Helsinki 1 DC Park 1' },
  { code: 'ash', city: 'Ashburn', country: 'US', description: 'Ashburn DC1' },
  { code: 'hil', city: 'Hillsboro', country: 'US', description: 'Hillsboro DC1' },
  { code: 'sin', city: 'Singapore', country: 'SG', description: 'Singapore DC1' },
];
const EUROPE: readonly string[] = ['nbg1', 'fsn1', 'hel1'];

type Type = readonly [
  sku: string,
  cpuType: 'shared' | 'dedicated',
  arch: 'x86' | 'arm',
  cores: number,
  memoryGb: number,
  diskGb: number,
  hourly: string,
  monthly: string,
  europeOnly: boolean,
];

/** The design's fifteen types (design-source/AppRafterApp.dc.html, `catalogue`), net prices. */
const TYPES: readonly Type[] = [
  ['cx22', 'shared', 'x86', 2, 4, 40, '0.0060', '3.79', true],
  ['cx32', 'shared', 'x86', 4, 8, 80, '0.0110', '6.80', true],
  ['cx42', 'shared', 'x86', 8, 16, 160, '0.0260', '16.40', true],
  ['cx52', 'shared', 'x86', 16, 32, 320, '0.0520', '32.40', true],
  ['cpx11', 'shared', 'x86', 2, 2, 40, '0.0070', '4.35', false],
  ['cpx21', 'shared', 'x86', 3, 4, 80, '0.0120', '7.55', false],
  ['cpx31', 'shared', 'x86', 4, 8, 160, '0.0220', '13.60', false],
  ['cpx41', 'shared', 'x86', 8, 16, 240, '0.0400', '25.20', false],
  ['cax11', 'shared', 'arm', 2, 4, 40, '0.0060', '3.79', true],
  ['cax21', 'shared', 'arm', 4, 8, 80, '0.0100', '6.49', true],
  ['cax31', 'shared', 'arm', 8, 16, 160, '0.0200', '12.49', true],
  ['cax41', 'shared', 'arm', 16, 32, 320, '0.0390', '24.49', true],
  ['ccx13', 'dedicated', 'x86', 2, 8, 80, '0.0200', '12.49', false],
  ['ccx23', 'dedicated', 'x86', 4, 16, 160, '0.0390', '24.49', false],
  ['ccx33', 'dedicated', 'x86', 8, 32, 240, '0.0770', '48.49', false],
];

function offer(location: string, type: Type): MachineOfferView {
  const [sku, cpuType, arch, cores, memoryGb, diskGb, hourly, monthly] = type;
  return {
    location,
    sku,
    cores,
    memoryGb,
    diskGb,
    arch,
    cpuType,
    priceMonthlyNet: monthly,
    priceHourlyNet: hourly,
    // Sold out in Nuremberg for now.
    available: !(sku === 'cpx31' && location === 'nbg1'),
    recommended: sku === 'cx32',
    // Announced to retire, still orderable until then.
    deprecation:
      sku === 'cpx11' ? { announced: '2026-09-01', unavailableAfter: '2026-12-01' } : null,
    retired: false,
  };
}

/**
 * Six regions; in Europe every one of the design's types, elsewhere the ones not Europe-only (the
 * design's `eu` flag); cx32 the recommended type, cpx31 sold out in nbg1, cpx11 retiring after
 * 2026-12-01; and cx11 in nbg1, retired, which the core still lists.
 */
export const MOCK_CATALOGUE: MachineCatalogue = {
  regions: [...REGIONS],
  offers: REGIONS.flatMap(({ code }) => [
    ...TYPES.filter((type) => EUROPE.includes(code) || !type[8]).map((type) => offer(code, type)),
    ...(code === 'nbg1'
      ? [
          {
            location: 'nbg1',
            sku: 'cx11',
            cores: 1,
            memoryGb: 2,
            diskGb: 20,
            arch: 'x86',
            cpuType: 'shared',
            priceMonthlyNet: '3.29',
            priceHourlyNet: '0.0053',
            available: false,
            recommended: false,
            deprecation: { announced: '2025-06-01', unavailableAfter: '2025-09-01' },
            retired: true,
          } satisfies MachineOfferView,
        ]
      : []),
  ]),
};

/** Each region's round trip from this computer; Singapore did not answer. */
export const MOCK_LATENCIES: readonly RegionLatency[] = [
  { region: 'nbg1', latencyMs: 38 },
  { region: 'fsn1', latencyMs: 41 },
  { region: 'hel1', latencyMs: 12 },
  { region: 'ash', latencyMs: 96 },
  { region: 'hil', latencyMs: 168 },
  { region: 'sin', latencyMs: null },
];

// The toolchain ---------------------------------------------------------------------------------

type ToolSpec = readonly [
  tool: ToolId,
  required: boolean,
  purpose: string,
  dir: string,
  version: string,
  install: readonly (readonly [HintOs, string])[],
];

/** The core's tool specs (cli-core tools.rs), in its probe order, as found on a Mac. */
const TOOLS: readonly ToolSpec[] = [
  [
    'kubectl',
    true,
    'talking to the cluster',
    '/opt/homebrew/bin',
    'Client Version: v1.33.1',
    [
      ['macos', 'brew install kubectl'],
      ['debian', 'apt install kubectl'],
      ['nix', 'nix profile install nixpkgs#kubectl'],
      ['windows', 'winget install Kubernetes.kubectl'],
      ['other', 'https://kubernetes.io/docs/tasks/tools/'],
    ],
  ],
  [
    'helm',
    false,
    'installing platform charts',
    '/opt/homebrew/bin',
    'v3.18.2+g04cad46',
    [
      ['macos', 'brew install helm'],
      ['debian', 'apt install helm'],
      ['nix', 'nix profile install nixpkgs#kubernetes-helm'],
      ['windows', 'winget install Helm.Helm'],
      ['other', 'https://helm.sh/docs/intro/install/'],
    ],
  ],
  [
    'restic',
    false,
    'backup and restore',
    '/opt/homebrew/bin',
    'restic 0.18.0',
    [
      ['macos', 'brew install restic'],
      ['debian', 'apt install restic'],
      ['windows', 'winget install restic.restic'],
      ['other', 'https://restic.readthedocs.io/en/stable/020_installation.html'],
    ],
  ],
  [
    'git',
    false,
    'reading the application repository',
    '/usr/bin',
    'git version 2.50.1',
    [
      ['macos', 'xcode-select --install'],
      ['windows', 'winget install Git.Git'],
      ['other', 'https://git-scm.com/downloads'],
    ],
  ],
  [
    'ssh',
    false,
    'reaching the node over SSH',
    '/usr/bin',
    'OpenSSH_10.0p2',
    [
      ['macos', 'preinstalled'],
      ['windows', 'built into Windows 10/11: Settings › Optional features › OpenSSH Client'],
    ],
  ],
  [
    'cue',
    false,
    'validating application manifests',
    '/opt/homebrew/bin',
    'cue version v0.17.1',
    [
      ['macos', 'brew install cue'],
      ['windows', 'winget install CueLang.Cue'],
      ['other', 'https://cuelang.org/docs/introduction/installation/'],
    ],
  ],
];

const SEARCH: Pick<ToolchainReport, 'searchPath' | 'searchPathSource'> = {
  searchPath: ['/opt/homebrew/bin', '/usr/bin', '/bin'],
  searchPathSource: 'login_shell',
};

function toolStatus([tool, required, purpose, dir, version, install]: ToolSpec, found: boolean) {
  return {
    tool,
    required,
    purpose,
    path: found ? `${dir}/${tool}` : null,
    version: found ? version : null,
    problem: found ? null : { kind: 'not_found' },
    install: install.map(([os, command]) => ({ os, command })),
  } satisfies ToolStatus;
}

/** Every tool found but helm, which is missing (the doctor's helm row opens this). */
export const MOCK_TOOLCHAIN: ToolchainReport = {
  tools: TOOLS.map((spec) => toolStatus(spec, spec[0] !== 'helm')),
  ...SEARCH,
};

/**
 * Every tool found (`?tools=found`): the panel with nothing to install, whose rows hold no
 * control — at the smallest window its body scrolls.
 */
export const MOCK_TOOLCHAIN_FOUND: ToolchainReport = {
  tools: TOOLS.map((spec) => toolStatus(spec, true)),
  ...SEARCH,
};

// Doctor ----------------------------------------------------------------------------------------

const row = (check: Pick<Check, 'id' | 'status' | 'title'> & Partial<Check>): Check => ({
  tool: null,
  detail: null,
  fix: null,
  ...check,
});

/** The SSH key row as the core's doctor reads the stored key (apprafter-core doctor.rs). */
function sshKeyRow(report: TargetReport): Check {
  const key = report.sshKey;
  const target = report.name;
  const title = 'SSH key readable';
  if (key === null) {
    return row({
      id: 'ssh_key',
      status: 'warn',
      title: 'SSH key path configured',
      fix: { kind: 'configure_ssh_key', target },
    });
  }
  const detail = key.display;
  switch (key.problem) {
    case null:
      return row({ id: 'ssh_key', status: 'pass', title, detail: `${detail} (${key.algo})` });
    case 'missing':
      return row({
        id: 'ssh_key',
        status: 'fail',
        title,
        detail,
        fix: { kind: 'ssh_key_missing', target, path: key.path },
      });
    case 'unreadable':
      return row({
        id: 'ssh_key',
        status: 'fail',
        title,
        detail,
        fix: { kind: 'explain', text: 'cannot read file: Is a directory (os error 21)' },
      });
    default:
      return row({
        id: 'ssh_key',
        status: 'fail',
        title,
        detail,
        fix: {
          kind: 'ssh_key_not_public',
          target,
          path: key.path,
          privateKey: key.problem === 'private_key',
        },
      });
  }
}

/**
 * What the doctor finds for a target the store holds, row for row as the core writes it; a
 * provisioned one shows every status.
 */
export function mockDoctorReport(report: TargetReport): DoctorReport {
  const target = report.name;
  const server = report.provisioned.status === 'provisioned' ? report.provisioned.server : null;
  const cluster: Check[] =
    server === null
      ? [
          ['kubeconfig_cached', 'Kubeconfig cached'] as const,
          ['kube_api_reachable', 'Kube API reachable'] as const,
          ['node_ssh_reachable', 'Node reachable over SSH'] as const,
        ].map(([id, title]) =>
          row({ id, status: 'skipped', title, detail: 'no provisioned server' }),
        )
      : // As the core's cluster rows read a state with no kubeconfig cached: nothing to probe the
        // API with, and the node's SSH port not answering.
        [
          row({
            id: 'kubeconfig_cached',
            status: 'fail',
            title: 'Kubeconfig cached',
            detail: `none cached for server \`${server.serverName}\` (id ${server.serverId})`,
            fix: { kind: 'fetch_kubeconfig', target },
          }),
          row({
            id: 'kube_api_reachable',
            status: 'skipped',
            title: 'Kube API reachable',
            detail: 'no cached kubeconfig',
          }),
          row({
            id: 'node_ssh_reachable',
            status: 'fail',
            title: 'Node reachable over SSH',
            detail: 'port 22 · 203.0.113.17: connection timed out',
            fix: { kind: 'node_unreachable', address: '203.0.113.17' },
          }),
        ];
  const tools = MOCK_TOOLCHAIN.tools.map((tool) =>
    row({
      id: 'tool',
      tool: tool.tool,
      status: tool.problem === null ? 'pass' : tool.required ? 'fail' : 'warn',
      title: `\`${tool.tool}\` on PATH`,
      detail: tool.version,
      fix: tool.problem === null ? null : { kind: 'install_tool', tool: tool.tool },
    }),
  );
  return {
    target,
    groups: [
      {
        id: 'target',
        checks: [
          row({
            id: 'config_readable',
            status: 'pass',
            title: 'Config file readable',
            detail: report.configFile,
          }),
          row({
            id: 'token_verified',
            status: 'pass',
            title: 'Token verified against provider API',
            detail: 'Hetzner Cloud /v1/locations, 182 ms',
          }),
          sshKeyRow(report),
        ],
      },
      { id: 'cluster', checks: cluster },
      {
        id: 'this_computer',
        checks: [
          ...tools,
          row({
            id: 'dns',
            status: 'pass',
            title: 'DNS resolves `api.hetzner.cloud`',
            detail: '443/tcp',
          }),
        ],
      },
    ],
  };
}

const stage = (index: number, total: number, title: string): OpEvent => ({
  kind: 'stage',
  index,
  total,
  title,
});

const result = (value: unknown) => ({ result: value as JsonValue });

/**
 * The richer answers, over D.3d's engine and store: the four reads above, and the clipboard. A
 * doctor for a target the store does not hold goes to `fallback` (D.3d's answers), whose report
 * offers Add a target.
 */
export function flowHandlers(
  ops: MockOps,
  store: MockStore,
  fallback: Record<string, Handler>,
): Record<string, Handler> {
  const read = (title: string, target: string | null, run: MockRun) =>
    ops.startRead(title, target, run);
  return {
    op_start_machine_catalogue: (args) => {
      const { source } = args as { source: CatalogueSourceArg };
      // Refused at the command, as Rust does: no read starts.
      const refused = catalogueSourceRefusal(source, store);
      if (refused !== null) return Promise.reject(refused);
      const [title, target] =
        source.kind === 'target'
          ? [`Machine catalogue · ${source.name}`, source.name]
          : ['Machine catalogue', null];
      return read(title, target, { end: () => result(structuredClone(MOCK_CATALOGUE)) });
    },
    op_start_region_latencies: (args) => {
      const { regions } = args as { regions: string[] };
      return read('Region latency', null, {
        end: () =>
          result(
            regions.map(
              (region): RegionLatency =>
                MOCK_LATENCIES.find((l) => l.region === region) ?? { region, latencyMs: null },
            ),
          ),
      });
    },
    op_start_doctor: (args) => {
      const { target } = args as { target: string };
      const report = store.reports.get(target);
      if (report === undefined) return fallback.op_start_doctor?.(args);
      return read(`Doctor · ${target}`, target, {
        events: [stage(1, 3, 'Target'), stage(2, 3, 'Cluster'), stage(3, 3, 'This computer')],
        // What the store holds when the run ends, as the core reads the files then.
        end: () => result(mockDoctorReport(store.reports.get(target) ?? report)),
      });
    },
    toolchain_status: () => structuredClone(MOCK_TOOLCHAIN),
    // Write-only, as the app's capability: Playwright reads what was written here.
    'plugin:clipboard-manager|write_text': (args) => {
      (window as unknown as { __mockClipboard?: string }).__mockClipboard = (
        args as { text: string }
      ).text;
      return null;
    },
  };
}
