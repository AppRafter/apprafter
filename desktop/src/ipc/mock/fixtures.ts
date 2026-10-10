// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The mock IPC's data (`bun run dev:mock`, the Playwright smoke, the mock's own tests), typed by
// the generated IPC types. Mock data lives here only, never in production code. Nothing here is
// token-shaped: a draft names its provider, never a token.

import type { DoctorReport } from '../generated/DoctorReport';
import type { MachineCatalogue } from '../generated/MachineCatalogue';
import type { SshKeyCandidate } from '../generated/SshKeyCandidate';
import type { SshKeyInfo } from '../generated/SshKeyInfo';
import type { TargetReport } from '../generated/TargetReport';
import type { ToolchainReport } from '../generated/ToolchainReport';
import type { UnreadableTarget } from '../generated/UnreadableTarget';
import type { WhoamiReport } from '../generated/WhoamiReport';

/** The CLI's default target in a fresh mock store. */
export const MOCK_CLI_DEFAULT = 'prod-eu';

/** The demo's home directory, the context's `home_dir`: what a typed `~/` expands into. */
export const MOCK_HOME = '/home/alex';
const HOME = MOCK_HOME;
/** A target's two files in the store, as `target_show` reports them: they follow its name. */
export const targetFiles = (name: string) => ({
  configFile: `~/.config/apprafter/targets/${name}/config.yaml`,
  credentialsFile: `~/.config/apprafter/targets/${name}/credentials.yaml`,
});

/**
 * The store's readable targets, as `target_show` reports them: prod-eu (provisioned as
 * prod-eu-1, id 4711), staging (not provisioned), lab (no server type, no tier, its SSH key
 * missing).
 */
export const MOCK_REPORTS: readonly TargetReport[] = [
  {
    name: 'prod-eu',
    isCliDefault: true,
    provider: 'hetzner-cloud',
    region: 'nbg1',
    serverType: 'cx22',
    defaultTier: 'team',
    tierLevel: 2,
    clusterName: null,
    sshKey: {
      path: `${HOME}/.ssh/id_ed25519.pub`,
      display: '~/.ssh/id_ed25519.pub',
      exists: true,
      algo: 'ssh-ed25519',
      problem: null,
    },
    token: { set: true, chars: 64 },
    ...targetFiles('prod-eu'),
    provisioned: {
      status: 'provisioned',
      server: { serverId: 4711, serverName: 'prod-eu-1', serverType: 'cpx22' },
    },
  },
  {
    name: 'staging',
    isCliDefault: false,
    provider: 'hetzner-cloud',
    region: 'fsn1',
    serverType: 'cx22',
    defaultTier: 'solo',
    tierLevel: 1,
    clusterName: null,
    sshKey: {
      path: `${HOME}/.ssh/id_ed25519.pub`,
      display: '~/.ssh/id_ed25519.pub',
      exists: true,
      algo: 'ssh-ed25519',
      problem: null,
    },
    token: { set: true, chars: 64 },
    ...targetFiles('staging'),
    provisioned: { status: 'not_provisioned' },
  },
  {
    name: 'lab',
    isCliDefault: false,
    provider: 'hetzner-cloud',
    region: 'hel1',
    serverType: null,
    defaultTier: null,
    tierLevel: null,
    clusterName: null,
    sshKey: {
      path: `${HOME}/.ssh/lab.pub`,
      display: '~/.ssh/lab.pub',
      exists: false,
      algo: null,
      problem: 'missing',
    },
    token: { set: true, chars: 64 },
    ...targetFiles('lab'),
    provisioned: { status: 'not_provisioned' },
  },
];

/** A target whose files could not be read: listed apart, never hidden. */
export const MOCK_UNREADABLE: readonly UnreadableTarget[] = [
  {
    name: 'broken',
    error: {
      code: 'apprafter::target::invalid_config',
      message: 'targets/broken/config.yaml: expected a mapping',
      help: null,
      causes: [],
      fields: { path: 'targets/broken/config.yaml' },
    },
  },
];

/**
 * What under the demo's home is not a public key, as `ssh_key_inspect` reads it: the private half
 * of id_ed25519 (one dropped `.pub` away from it), a note, and the two directories a typed path
 * can name (`~/` is the home itself), which cannot be read as a file.
 */
export const MOCK_NOT_KEYS: readonly SshKeyInfo[] = [
  { path: HOME, display: '~/', exists: true, algo: null, problem: 'unreadable' },
  { path: `${HOME}/.ssh`, display: '~/.ssh', exists: true, algo: null, problem: 'unreadable' },
  {
    path: `${HOME}/.ssh/id_ed25519`,
    display: '~/.ssh/id_ed25519',
    exists: true,
    algo: null,
    problem: 'private_key',
  },
  {
    path: `${HOME}/notes.txt`,
    display: '~/notes.txt',
    exists: true,
    algo: null,
    problem: 'not_public_key',
  },
];

/**
 * The `.pub` files the key picker finds under `~/.ssh`, by path: two public keys, and old.pub,
 * which holds none (the core names no type for it).
 */
export const MOCK_SSH_KEYS: readonly SshKeyCandidate[] = [
  {
    path: `${HOME}/.ssh/id_ed25519.pub`,
    display: '~/.ssh/id_ed25519.pub',
    algo: 'ssh-ed25519',
    comment: 'alex@workstation',
  },
  {
    path: `${HOME}/.ssh/old.pub`,
    display: '~/.ssh/old.pub',
    algo: null,
    comment: null,
  },
  {
    path: `${HOME}/.ssh/work.pub`,
    display: '~/.ssh/work.pub',
    algo: 'ssh-rsa',
    comment: 'alex@work',
  },
];

/** The tools on a Mac with Homebrew: kubectl and helm found, restic missing. */
export const MOCK_TOOLCHAIN: ToolchainReport = {
  tools: [
    {
      tool: 'kubectl',
      required: true,
      purpose: 'talks to the cluster',
      path: '/opt/homebrew/bin/kubectl',
      version: 'Client Version: v1.33.1',
      problem: null,
      install: [{ os: 'macos', command: 'brew install kubernetes-cli' }],
    },
    {
      tool: 'helm',
      required: true,
      purpose: 'installs charts',
      path: '/opt/homebrew/bin/helm',
      version: 'v3.18.2+g04cad46',
      problem: null,
      install: [{ os: 'macos', command: 'brew install helm' }],
    },
    {
      tool: 'restic',
      required: false,
      purpose: 'backs up and restores',
      path: null,
      version: null,
      problem: { kind: 'not_found' },
      install: [{ os: 'macos', command: 'brew install restic' }],
    },
  ],
  searchPath: ['/opt/homebrew/bin', '/usr/bin', '/bin'],
  searchPathSource: 'login_shell',
};

/** Two regions and three offers. */
export const MOCK_CATALOGUE: MachineCatalogue = {
  regions: [
    { code: 'hel1', city: 'Helsinki', country: 'FI', description: 'Helsinki DC Park 1' },
    { code: 'nbg1', city: 'Nuremberg', country: 'DE', description: 'Nuremberg 1 DC 3' },
  ],
  offers: [
    {
      location: 'nbg1',
      sku: 'cx22',
      cores: 2,
      memoryGb: 4,
      diskGb: 40,
      arch: 'x86',
      cpuType: 'shared',
      priceMonthlyNet: '3.79',
      priceHourlyNet: '0.0060',
      available: true,
      recommended: true,
      deprecation: null,
      retired: false,
    },
    {
      location: 'nbg1',
      sku: 'cx32',
      cores: 4,
      memoryGb: 8,
      diskGb: 80,
      arch: 'x86',
      cpuType: 'shared',
      priceMonthlyNet: '6.80',
      priceHourlyNet: '0.0109',
      available: true,
      recommended: false,
      deprecation: null,
      retired: false,
    },
    {
      location: 'hel1',
      sku: 'cx22',
      cores: 2,
      memoryGb: 4,
      diskGb: 40,
      arch: 'x86',
      cpuType: 'shared',
      priceMonthlyNet: '3.79',
      priceHourlyNet: '0.0060',
      available: true,
      recommended: true,
      deprecation: null,
      retired: false,
    },
  ],
};

/** Each region's round trip, in milliseconds; a region not listed did not answer. */
export const MOCK_LATENCIES: Readonly<Record<string, number>> = { nbg1: 24, fsn1: 27, hel1: 41 };

/** Doctor on prod-eu: three groups, one row each. */
export const MOCK_DOCTOR: DoctorReport = {
  target: 'prod-eu',
  groups: [
    {
      id: 'target',
      checks: [
        {
          id: 'token_verified',
          tool: null,
          status: 'pass',
          title: 'Token accepted by Hetzner Cloud',
          detail: 'Hetzner Cloud /v1/locations, 182 ms',
          fix: null,
        },
      ],
    },
    {
      id: 'cluster',
      checks: [
        {
          id: 'kubeconfig_cached',
          tool: null,
          status: 'warn',
          title: 'Kubeconfig cached',
          detail: null,
          fix: { kind: 'fetch_kubeconfig', target: 'prod-eu' },
        },
      ],
    },
    {
      id: 'this_computer',
      checks: [
        {
          id: 'tool',
          tool: 'kubectl',
          status: 'pass',
          title: '`kubectl` on PATH',
          detail: 'Client Version: v1.33.1',
          fix: null,
        },
      ],
    },
  ],
};

/** whoami with a ping: the CLI default, prod-eu, its token accepted. */
export const MOCK_WHOAMI: WhoamiReport = {
  identity: 'anonymous_self_hosted',
  cliDefault: {
    status: 'found',
    target: {
      name: 'prod-eu',
      provider: 'hetzner-cloud',
      verification: { status: 'verified', elapsedMs: 182 },
      region: 'nbg1',
      serverType: 'cx22',
      defaultTier: 'team',
      clusterName: null,
      sshKey: {
        path: `${HOME}/.ssh/id_ed25519.pub`,
        display: '~/.ssh/id_ed25519.pub',
        exists: true,
        algo: 'ssh-ed25519',
        problem: null,
      },
    },
  },
};
