// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { type ErrorAction, errorAction } from './errors';
import { DESKTOP_ERROR_CODES } from './generated/errors';
import type { JsonValue } from './generated/serde_json/JsonValue';
import type { UiError } from './generated/UiError';

const error = (code: string | null, fields: Record<string, JsonValue> = {}): UiError => ({
  code,
  message: 'm',
  help: null,
  causes: [],
  fields,
});

// Spec §5.1.
const rows: [string, UiError, ErrorAction['kind']][] = [
  ['target not found', error('apprafter::target::not_found'), 'add-target'],
  ['token rejected', error('apprafter::target::token_rejected'), 'renew-token'],
  ['Hetzner 401', error('apprafter::provider::hetzner_api_error', { status: 401 }), 'renew-token'],
  ['Hetzner 403', error('apprafter::provider::hetzner_api_error', { status: 403 }), 'none'],
  ['Hetzner, no status', error('apprafter::provider::hetzner_api_error'), 'none'],
  [
    'Hetzner, status as text',
    error('apprafter::provider::hetzner_api_error', { status: '401' }),
    'none',
  ],
  ['a tool missing', error('apprafter::env::tool_not_found'), 'toolchain'],
  ['cue missing', error('apprafter::env::cue_not_found'), 'toolchain'],
  [
    'server type unavailable',
    error('apprafter::provider::server_type_unavailable'),
    'machine-picker',
  ],
  ['state corrupt', error('apprafter::state::corrupt'), 'import'],
  ['backup job active', error('apprafter::backup::job_active'), 'backup-status'],
  ['target busy', error('apprafter::target::busy'), 'running-op'],
  ['locked: the gate shows the lock', error(DESKTOP_ERROR_CODES.LOCKED), 'none'],
  ['any other code', error('apprafter::op::cancelled'), 'none'],
  ['no code', error(null), 'none'],
  ['a code named like an Object member', error('toString'), 'none'],
];

test.each(rows)('%s', (_, ui, kind) => {
  expect(errorAction(ui)).toEqual({ kind });
});
