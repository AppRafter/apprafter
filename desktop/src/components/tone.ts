// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The design's tone() map (brief §1.5): text and background from the ok/warn/err tokens,
// neutral from --fg-muted on --surface-2. Health: Healthy ok, Progressing warn, Degraded err;
// plan class: destructive err, bounded warn, else neutral.
export type Tone = 'ok' | 'warn' | 'err' | 'neutral';
