// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Targets for the Targets view in mock mode, typed by hand until D.3 adds `target_list`. Mock
// data lives here only, never in production code.

export interface MockTarget {
  readonly name: string;
  readonly provider: string;
  readonly region: string;
  /** Hardware tier: 1 Solo, 2 Team, 3 Pro, 4 Regulated. */
  readonly tier: 1 | 2 | 3 | 4;
  /** The CLI's default target. */
  readonly cliDefault: boolean;
}

export const MOCK_TARGETS: readonly MockTarget[] = [
  { name: 'prod-eu', provider: 'hetzner-cloud', region: 'nbg1', tier: 2, cliDefault: true },
  { name: 'staging', provider: 'hetzner-cloud', region: 'fsn1', tier: 1, cliDefault: false },
  { name: 'lab', provider: 'hetzner-cloud', region: 'hel1', tier: 1, cliDefault: false },
];
