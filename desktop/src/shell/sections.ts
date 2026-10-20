// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The sections of a target tab (brief §2): their nav entries, the design's page copy, and the
// slice that brings each screen — the slice coverage.toml gives the CLI command behind it
// (sections.test.ts keeps the two equal).
import {
  ArchiveIcon,
  CubeIcon,
  DatabaseIcon,
  GlobeHemisphereWestIcon,
  HardDriveIcon,
  type Icon,
  PlugIcon,
  SealCheckIcon,
  SquaresFourIcon,
  StackIcon,
} from '../components/icons';
import type { Section } from '../state/session';

export interface SectionInfo {
  readonly id: Section;
  readonly label: string;
  readonly icon: Icon;
  readonly group: 'main' | 'manage';
  /** The page subtitle, from the design; the target name where it needs one. */
  readonly sub?: (target: string) => string;
  /** The CLI command whose read view the screen is, and the slice that brings it. */
  readonly leaf: string;
  readonly slice: string;
  /** The section has its screen: its leaf is a GUI action in coverage.toml. */
  readonly screen?: true;
  /**
   * What the placeholder says until the screen arrives. A planned section's command runs on the
   * CLI's active target, so the hint says to switch to the tab's first (sections.test.ts checks
   * it against the reference).
   */
  readonly planned?: string;
}

export const SECTIONS: readonly SectionInfo[] = [
  {
    id: 'overview',
    label: 'Overview',
    icon: SquaresFourIcon,
    group: 'main',
    leaf: 'status',
    slice: 'D.5',
    planned: 'The overview arrives in D.5.',
  },
  {
    id: 'apps',
    label: 'Applications',
    icon: CubeIcon,
    group: 'main',
    leaf: 'app list',
    slice: 'D.6',
    planned: 'Applications arrive in D.6.',
  },
  {
    id: 'approvals',
    label: 'Approvals',
    icon: SealCheckIcon,
    group: 'main',
    leaf: 'migration list',
    slice: 'D.7',
    planned: 'Approvals arrive in D.7.',
  },
  {
    id: 'backups',
    label: 'Backups',
    icon: ArchiveIcon,
    group: 'manage',
    sub: () =>
      'Encrypted off-site snapshots of claims data, platform objects and secrets. A restore replays one onto this or a fresh cluster.',
    leaf: 'backup status',
    slice: 'D.11',
    planned: 'Backups arrive in D.11.',
  },
  {
    id: 'data',
    label: 'Data',
    icon: DatabaseIcon,
    group: 'manage',
    sub: () =>
      'Databases, volumes, secrets and repository credentials that live beside your applications. Apps can only bind what sits in their own namespace.',
    leaf: 'db list',
    slice: 'D.8',
    planned: 'Data arrives in D.8.',
  },
  {
    id: 'network',
    label: 'Networking',
    icon: GlobeHemisphereWestIcon,
    group: 'manage',
    sub: () =>
      'Public ingress through the platform Gateway — domains, TLS, the origin firewall — and what applications may reach outbound.',
    leaf: 'target domain list',
    slice: 'D.9',
    planned: 'Networking arrives in D.9.',
  },
  {
    id: 'platform',
    label: 'Platform',
    icon: StackIcon,
    group: 'manage',
    sub: () =>
      'One version for every platform component, held by the PlatformStack, plus the defaults that apply cluster-wide.',
    leaf: 'platform status',
    slice: 'D.5',
    planned: 'The platform view arrives in D.5.',
  },
  {
    id: 'nodes',
    label: 'Nodes',
    icon: HardDriveIcon,
    group: 'manage',
    sub: () =>
      'Where capacity goes on each node, and its swap posture. Preparing a node retrofits kubelet reservations, OOM protection for k3s and a host swapfile.',
    leaf: 'node status',
    slice: 'D.5',
    planned: 'Nodes arrive in D.5.',
  },
  {
    id: 'target',
    label: 'Target',
    icon: PlugIcon,
    group: 'manage',
    sub: (target) =>
      `How this computer reaches ${target} — provider credentials, the machine, and cached access to the cluster.`,
    leaf: 'target show',
    slice: 'D.3',
    screen: true,
  },
];

export function sectionInfo(id: Section): SectionInfo {
  const info = SECTIONS.find((s) => s.id === id);
  if (info === undefined) throw new Error(`no section ${id}`);
  return info;
}
