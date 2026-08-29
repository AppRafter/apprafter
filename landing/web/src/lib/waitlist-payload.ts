// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Splits the waitlist form's flat `selected` map into the two fields
// WaitlistSignups declares. They hold different sets:
//
//   `interests` — the checkboxes the form rendered (waitlistCopy.interests)
//   `phases`    — stable roadmap ids from the registry (phases.json)
//
// A roadmap "Get notified when <phase> ships" button preselects a phase
// id that need not have a checkbox. Posting such an id as an `interest`
// sends a value outside that select's options: Payload rejects the whole
// signup, and the form's 400-as-already-signed-up branch then shows the
// visitor a success message while nothing is written. `federation` was
// exactly that — present in the `phases` enum, absent from `interests`.
//
// PHASE_IDS is derived from the registry rather than restated, so adding
// a phase cannot reintroduce the divergence. It replaces a hand-kept
// fourth copy of the id list that nothing compared against phases.json.

import registry from '../data/phases.json';

export const PHASE_IDS: ReadonlySet<string> = new Set(
  (registry.phases as { id: string }[]).map((p) => p.id),
);

export type InterestOption = { key: string };

// `undefined` is spelled out because the web tsconfig sets
// exactOptionalPropertyTypes. Both fields are handed straight to
// JSON.stringify, which drops an undefined value rather than sending
// null — an empty selection must post no field at all.
export type WaitlistFields = {
  interests?: string[] | undefined;
  phases?: string[] | undefined;
};

/**
 * `selected` carries every key the form has set true — rendered
 * checkboxes and preselected phase ids alike. Each output field takes
 * only the keys it can legally hold: a key that is both (`tier2`) lands
 * in both, and a key that is neither is dropped rather than posted.
 */
export function waitlistFields(
  selected: Record<string, boolean>,
  interestOptions: readonly InterestOption[] | null | undefined,
): WaitlistFields {
  const chosen = Object.entries(selected)
    .filter(([, v]) => v)
    .map(([k]) => k);
  const offered = new Set((interestOptions ?? []).map((o) => o.key));

  const interests = chosen.filter((k) => offered.has(k));
  const phases = chosen.filter((k) => PHASE_IDS.has(k));

  return {
    interests: interests.length ? interests : undefined,
    phases: phases.length ? phases : undefined,
  };
}
