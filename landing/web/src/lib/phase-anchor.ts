// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Single source of the roadmap anchor slug. Derived from the STABLE
// registry id (phases.json), never from the display label — a
// label-derived slug produced the doubled `roadmap-phase-phase-N`
// (R2). Consumed by Roadmap.astro and PhaseChip.astro; the comparison
// table's inbound cross-link must match its output.

export function phaseAnchor(id: string): string {
  return `roadmap-phase-${id.toLowerCase().replace(/\W+/g, '-').replace(/^-|-$/g, '')}`;
}
