// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { type ErrorAction, errorAction } from './errors';
import { CORE_ERROR_CODES } from './generated/core-errors';
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
  ['locked: the gate shows the lock', error(DESKTOP_ERROR_CODES.LOCKED), 'none'],
  ['any other code', error('apprafter::op::cancelled'), 'none'],
  ['no code', error(null), 'none'],
  ['a code named like an Object member', error('toString'), 'none'],
];

test.each(rows)('%s', (_, ui, kind) => {
  expect(errorAction(ui)).toEqual({ kind });
});

test('the actions key on the generated codes, not on a hand list', () => {
  expect(errorAction(error(CORE_ERROR_CODES.TARGET_NOT_FOUND)).kind).toBe('add-target');
  expect(errorAction(error(CORE_ERROR_CODES.TARGET_TOKEN_REJECTED)).kind).toBe('renew-token');
  expect(errorAction(error(CORE_ERROR_CODES.PROVIDER_SERVER_TYPE_UNAVAILABLE)).kind).toBe(
    'machine-picker',
  );
  expect(errorAction(error(CORE_ERROR_CODES.ENV_TOOL_NOT_FOUND)).kind).toBe('toolchain');
  expect(errorAction(error(CORE_ERROR_CODES.ENV_CUE_NOT_FOUND)).kind).toBe('toolchain');
  expect(errorAction(error(CORE_ERROR_CODES.STATE_CORRUPT)).kind).toBe('import');
  expect(errorAction(error(CORE_ERROR_CODES.BACKUP_JOB_ACTIVE)).kind).toBe('backup-status');
});

// Bug 11, the frontend half: the rules above run on the core's own projections
// (`UiError::from(&CoreError)`, exported by desktop/ipc/tests/export.rs), never only on
// hand-made objects. `tokenRejected` is what a ping answers a 401 with (verify, renew).
const real = (await Bun.file(
  new URL('./generated/fixtures/ui-errors.json', import.meta.url),
).json()) as Record<string, UiError>;

test.each([
  ['hetzner401', 'renew-token'],
  ['hetzner403', 'none'],
  ['tokenRejected', 'renew-token'],
  ['targetExists', 'none'],
  ['serverTypeUnavailable', 'machine-picker'],
  ['toolNotFound', 'toolchain'],
] as const)('the real projection of %s offers %s', (name, kind) => {
  const ui = real[name];
  expect(ui, name).toBeDefined();
  expect(errorAction(ui as UiError).kind).toBe(kind);
});

test('the real projections carry the fields the UI reads, in camelCase', () => {
  expect(real.hetzner401?.fields).toMatchObject({
    status: 401,
    endpoint: 'GET /v1/locations',
    apiCode: 'unauthorized',
  });
  expect(real.tokenRejected?.fields).toMatchObject({ provider: 'hetzner-cloud', status: 401 });
  expect(real.serverTypeUnavailable?.fields).toMatchObject({
    requested: 'cx22',
    location: 'nbg1',
    kind: 'retired',
    context: 'target_add',
  });
  expect(real.toolNotFound?.fields).toMatchObject({ tool: 'kubectl', neededBy: 'doctor' });
});
