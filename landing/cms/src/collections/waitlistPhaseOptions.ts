// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// Options for the WaitlistSignups `phases` select are GENERATED from the
// single WS-1 registry so adding a phase needs no schema edit. ids match
// the existing `interests` values, so pre-existing subscriptions stay valid.
// The JSON import is inlined by the bundler (the cms prod image does not
// carry phases.json), so no runtime file read is needed.
import registry from '../../../web/src/data/phases.json';

type Phase = { id: string; label: string; title: string; status: string; anchor: string };

export function phaseOptions(): { label: string; value: string }[] {
  return (registry.phases as Phase[])
    .filter((p) => p.status !== 'shipped')
    .map((p) => ({ label: `${p.label} — ${p.title}`, value: p.id }));
}
