// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Generated from apprafter-core and cli-core by `just desktop-ipc-types`. Do not edit.

/** The providers `target add` accepts. */
export const SUPPORTED_PROVIDERS = ['hetzner-cloud'] as const;

/** The default-tier hints, in order, with the hardware tier each names. */
export const TIERS = [
  { id: 'solo', level: 1 },
  { id: 'team', level: 2 },
  { id: 'prod', level: 3 },
  { id: 'regulated', level: 4 },
] as const;

/** The longest target name the core accepts, in UTF-8 bytes. */
export const TARGET_NAME_MAX_LEN = 64;

/** A Hetzner Cloud API token's length. */
export const HETZNER_TOKEN_LEN = 64;

/**
 * The rebuild of the provisioned target `name` on another machine, one command per line:
 * the CLI's own recipe (`apprafter_core::target::rebuild_recipe`).
 */
export const rebuildRecipe = (name: string): readonly string[] => [
  `apprafter target use ${name}`,
  `apprafter backup create --repo <repo>`,
  `apprafter destroy --yes`,
  `apprafter restore <repo> --reprovision --server-type <sku>`,
];
