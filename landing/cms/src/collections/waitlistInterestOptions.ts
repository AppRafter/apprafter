// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// The `interests` select's options, in their own module so the landing
// content gate can assert against the same list the collection declares
// rather than a copy. `phases` is generated from the registry
// (waitlistPhaseOptions); `interests` is hand-kept because it is the set
// of checkboxes the form renders, which is not the set of roadmap phases
// — `observability` is an interest and not a phase, `federation` is a
// phase and not an interest.
//
// Editing this list changes the Postgres enum: add a migration.

export const interestOptions: { label: string; value: string }[] = [
  { label: 'Tier 2 — production multi-node (Phase 3)', value: 'tier2' },
  { label: 'Observability (Phase 3)', value: 'observability' },
  { label: 'Managed offering (Phase 4)', value: 'managed' },
  { label: 'Tier 3 — bare metal (Phase 5+)', value: 'tier3' },
  { label: 'Tier 4 — confidential (Phase 6+)', value: 'tier4' },
];
