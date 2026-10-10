// SPDX-License-Identifier: FSL-1.1-Apache-2.0
/**
 * Every GUI action that runs a CLI leaf command. `desktop/coverage.toml` maps each visible
 * leaf of `docs/reference/cli/commands.json` either to one of these ids or to a status marker;
 * the coverage gate (coverage.test.ts) fails on a leaf with neither and on an id missing here.
 * Slices add their ids as they land (design spec §2, §5.3); D.2 ships none.
 */
export const ACTION_IDS = [] as const satisfies readonly string[];

export type ActionId = (typeof ACTION_IDS)[number];
