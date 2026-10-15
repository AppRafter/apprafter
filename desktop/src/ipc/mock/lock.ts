// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The target store's lock on the mock (D.4a §3.14): where its secrets are (`?secrets=files|locking|
// locked|unlocking`), the Security page's report, and the three flows — Protect, Finish, Stop
// protecting — on the ops engine, by Rust's rules where the page can tell: Protect runs only with its
// recovery key typed back, Finish only where a Protect was interrupted, Stop protecting asks for the
// store's secret. A protected store opens with MOCK_STORE_PASSWORD or MOCK_RECOVERY_KEY — T24's first
// test vector, a published value that opens nothing real. Copy copies nothing: the mock has no
// clipboard it may write a key to.
import type { InvokeArgs } from '@tauri-apps/api/core';
import { CORE_ERROR_CODES } from '../generated/core-errors';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import type { HolderHardening } from '../generated/HolderHardening';
import type { LockOffPlan } from '../generated/LockOffPlan';
import type { LockOnPlan } from '../generated/LockOnPlan';
import type { LockReport } from '../generated/LockReport';
import type { LockSectionInfo } from '../generated/LockSectionInfo';
import type { OpId } from '../generated/OpId';
import type { PlanView } from '../generated/PlanView';
import type { SecretsInfo } from '../generated/SecretsInfo';
import type { SecretsMode } from '../generated/SecretsMode';
import type { UiError } from '../generated/UiError';
import type { Handler, MockOps } from './ops';

/** The mock store's lock password: a demo value, nobody's. */
export const MOCK_STORE_PASSWORD = 'correct horse battery staple';

/** T24's first test vector (c1): the mock store's recovery key, and every new one the mock shows. */
export const MOCK_RECOVERY_KEY =
  'APPRAFTER-RECOVERY-KEY-1\nQQQSY-QCYQ5-RQWZQ-FPG9S-CRGWP-UGPZY-SNZS2-3V9CC-RYDPK-8QARC-0SY3K-4UK';

const STORE = '~/.config/apprafter';
const STORE_ID = '3f9c0a1b7d2e4c56';

/** A release build's protections on Linux, all taken. */
const HARDENED: HolderHardening = {
  nonDumpable: true,
  denyAttach: null,
  coreOff: true,
  memoryLocked: true,
  processDacl: null,
};

/** A recovery key as Rust compares it: case, spaces and dashes aside. */
const canonical = (key: string) => key.toLowerCase().replace(/[\s-]/g, '');

const uiError = (
  code: string | null,
  message: string,
  fields: UiError['fields'] = {},
): UiError => ({
  code,
  message,
  help: null,
  causes: [],
  fields,
});

const recoveryKeyInvalid = (reason: string) =>
  uiError(
    CORE_ERROR_CODES.SECRETS_RECOVERY_KEY_INVALID,
    `that recovery key is not right (${reason})`,
    {
      reason,
    },
  );

export interface MockLock {
  /** Where the store keeps its secrets, as app_info reports it. */
  readonly secrets: () => SecretsInfo;
  /** Whether `secret` opens the store: the lock password or the recovery key, as one field takes either. */
  readonly opens: (secret: unknown) => boolean;
  readonly handlers: Readonly<Record<string, Handler>>;
  /** A terminal's `apprafter lock on` under the app: the store is protected from now. */
  readonly lockFromTerminal: () => void;
  /** A terminal's `apprafter lock off` under the app: the secrets are in files from now. */
  readonly unlockFromTerminal: () => void;
}

/** As the core's `lock::on::title` names them: "targets: a, b", or "no targets". */
const namedTargets = (targets: readonly string[]) =>
  targets.length === 0 ? 'no targets' : `targets: ${targets.join(', ')}`;

/**
 * As Rust's core does, the report and the Protect plan's title name the store's targets as
 * they are at each call: `names` reads the mock store (targets.ts `namesIn`).
 */
export function mockLock(
  engine: MockOps,
  initial: SecretsMode,
  names: () => readonly string[],
): MockLock {
  let mode: SecretsMode = initial;
  /** The recovery key of each Protect plan: whether it was saved, and typed back. */
  const keys = new Map<OpId, { saved: boolean; confirmed: boolean }>();
  const notFound = (opId: unknown) =>
    uiError(DESKTOP_ERROR_CODES.PLAN_NOT_FOUND, `operation ${opId} has no pending plan`, {
      opId: opId as OpId,
    });
  const protectedNow = () => mode !== 'files';
  const opens = (secret: unknown) =>
    typeof secret === 'string' &&
    (secret === MOCK_STORE_PASSWORD || canonical(secret) === canonical(MOCK_RECOVERY_KEY));

  const report = (): LockReport => ({
    store: STORE,
    storeId: protectedNow() ? STORE_ID : null,
    // As Rust lists them (list_target_names): every name in the store, unreadable ones too, sorted.
    targets: [...names()],
    mode,
    slots: protectedNow()
      ? [
          { kind: 'password', label: 'lock password', refused: null },
          { kind: 'recovery_key', label: 'recovery key', refused: null },
        ]
      : [],
    windowMinutes: 5,
    window: protectedNow() ? { status: 'app' } : { status: 'closed' },
    // What detect_limits reports on Linux (critic A finding 15): scope 0 is the limit (1 to 3 are
    // not), and the same-user row is always there.
    limits: [
      { kind: 'ptrace_scope', value: 0 },
      { kind: 'same_user_programs' },
      { kind: 'secrets_in_use' },
      { kind: 'hibernation' },
    ],
  });

  const stages = (titles: readonly string[]) =>
    titles.map((title, i) => ({
      kind: 'stage' as const,
      index: i + 1,
      total: titles.length,
      title,
    }));

  const argsOf = (args: InvokeArgs | undefined) => (args ?? {}) as Record<string, unknown>;

  const handlers: Record<string, Handler> = {
    lock_info: (): LockSectionInfo => ({ report: report(), app: HARDENED }),
    op_plan_lock_on: (args): Promise<LockOnPlan> | LockOnPlan => {
      const { password } = argsOf(args);
      if (typeof password !== 'string' || password === '') {
        return Promise.reject(
          uiError(CORE_ERROR_CODES.SECRETS_PASSWORD_REFUSED, 'a lock password cannot be empty', {
            reason: 'empty',
          }),
        );
      }
      if (mode !== 'files') {
        return Promise.reject(uiError(null, "this target store's secrets are protected already"));
      }
      const entry = { saved: false, confirmed: false };
      const plan: PlanView = engine.registerPlan(
        {
          class: 'bounded',
          title: `Protect the target store at ${STORE} (${namedTargets(names())})`,
          changes: [
            {
              kind: 'Vault',
              object: `${STORE}/vault`,
              action: 'create',
              detail: 'opened by a lock password or a recovery key',
            },
            {
              kind: 'Credentials',
              object: 'prod-eu',
              action: 'move',
              detail: 'credentials.yaml → vault',
            },
          ],
          target: null,
        },
        {
          events: stages([
            'Writing the vault',
            'Checking the vault',
            'Re-encrypting cached secrets',
            'Removing the secrets from files',
          ]),
          end: () => {
            if (!entry.confirmed) return { error: recoveryKeyInvalid('not_confirmed') };
            mode = 'locked';
            return { result: { store: STORE, storeId: STORE_ID, alreadyLocked: false } };
          },
        },
      );
      keys.set(plan.opId, entry);
      return { plan, recoveryKey: MOCK_RECOVERY_KEY };
    },
    op_confirm_recovery_key: (args) => {
      const { opId, typed } = argsOf(args);
      const entry = keys.get(opId as OpId);
      if (entry === undefined) return Promise.reject(notFound(opId));
      if (!entry.saved) {
        return Promise.reject(
          uiError(null, 'the recovery key is typed back only once it is saved'),
        );
      }
      if (typeof typed !== 'string' || canonical(typed) !== canonical(MOCK_RECOVERY_KEY)) {
        // As Rust: a retype after the confirm is checked again, and the last one decides.
        entry.confirmed = false;
        return Promise.reject(recoveryKeyInvalid('mismatch'));
      }
      entry.confirmed = true;
      return null;
    },
    copy_recovery_key: (args) => {
      const { opId } = argsOf(args);
      const entry = keys.get(opId as OpId);
      if (entry === undefined) return Promise.reject(notFound(opId));
      if (entry.saved) {
        return Promise.reject(
          uiError(null, 'the recovery key is not copied again once it is saved'),
        );
      }
      return null;
    },
    op_recovery_key_saved: (args) => {
      const { opId } = argsOf(args);
      const entry = keys.get(opId as OpId);
      if (entry === undefined) return Promise.reject(notFound(opId));
      entry.saved = true;
      return null;
    },
    op_plan_lock_finish: (args) => {
      const { password, recovery } = argsOf(args);
      if (mode !== 'locking') {
        return Promise.reject(
          uiError(null, 'there is no interrupted Protect to finish in this target store'),
        );
      }
      // Rust opens each slot by its own kind (open_slot_at): the second field is a recovery key,
      // never the lock password again (critic A finding 16).
      const isKey =
        typeof recovery === 'string' && canonical(recovery) === canonical(MOCK_RECOVERY_KEY);
      if (password !== MOCK_STORE_PASSWORD || !isKey) {
        return Promise.reject(
          uiError(DESKTOP_ERROR_CODES.AUTH_FAILED, 'authentication failed', { exhausted: false }),
        );
      }
      return engine.registerPlan(
        {
          class: 'bounded',
          title: `Finish protecting the target store at ${STORE}`,
          changes: [],
          target: null,
        },
        {
          end: () => {
            mode = 'locked';
            return { result: { store: STORE, storeId: STORE_ID } };
          },
        },
      );
    },
    op_plan_lock_off: (): LockOffPlan => {
      // As Rust's plan_lock_off: a vault to open makes the plan destructive, its gesture the
      // store's secret. With nothing to open — a store in files, which the core plans as not
      // locked — it is a plain confirm (Bounded), as the CLI asks nothing there (critic B
      // finding 25; d4a-g run 4's concern 3).
      const vault = protectedNow();
      if (!vault) {
        const plan: PlanView = engine.registerPlan(
          {
            class: 'bounded',
            title: `The target store at ${STORE} is not locked`,
            changes: [],
            target: null,
          },
          {
            end: () => ({
              result: { store: STORE, notLocked: true, backedOut: false, ageKey: null },
            }),
          },
        );
        return { plan, needsKey: false };
      }
      // Stopping an interrupted Protect backs it out, as the core reports it (`backedOut`; the
      // page's toast says so: UI verifier M3).
      const backedOut = mode === 'locking';
      // The mock's store has no cached secret: no shared age key is used (d4a-f Task 3).
      const plan: PlanView = engine.registerPlan(
        {
          class: 'destructive',
          title: `Stop protecting the target store at ${STORE}`,
          changes: [
            {
              kind: 'Vault',
              object: `${STORE}/vault`,
              action: 'delete',
              detail: 'its secrets back in files',
            },
          ],
          target: null,
        },
        {
          events: stages([
            'Opening the vault',
            'Writing the files',
            'Checking the files',
            'Removing the vault',
          ]),
          end: () => {
            mode = 'files';
            keys.clear();
            return { result: { store: STORE, notLocked: false, backedOut, ageKey: null } };
          },
        },
        'store_secret',
      );
      return { plan, needsKey: false };
    },
  };

  return {
    secrets: () => ({
      mode,
      slots: protectedNow() ? ['password', 'recovery_key'] : [],
      protected: protectedNow(),
      store: STORE,
      problem: null,
    }),
    opens,
    handlers,
    lockFromTerminal: () => {
      mode = 'locked';
    },
    unlockFromTerminal: () => {
      mode = 'files';
    },
  };
}
