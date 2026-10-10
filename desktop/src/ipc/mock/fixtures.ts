// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Targets for the Targets view in mock mode (the design's three), until D.3 adds `target_list`.
// Mock data lives here only, never in production code: main.tsx hands it in, in mock mode.
import type { TargetSummary } from '../../screens/targets/targets';

export const MOCK_TARGETS: readonly TargetSummary[] = [
  { name: 'prod-eu', provider: 'hetzner-cloud', region: 'nbg1', tier: 2, cliDefault: true },
  { name: 'staging', provider: 'hetzner-cloud', region: 'fsn1', tier: 1, cliDefault: false },
  { name: 'lab', provider: 'hetzner-cloud', region: 'hel1', tier: 1, cliDefault: false },
];
