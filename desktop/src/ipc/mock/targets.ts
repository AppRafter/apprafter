// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The D.3 commands on the mock IPC, answering as Rust does (desktop/src-tauri/src/target_ops.rs
// over apprafter-core): the target store in memory, reads and plans on ops.ts's engine, and the
// rules the pages rely on in one place — the token's format and the demo's 401 (a well-formed
// token starting with `x`), a draft the add plan takes once it is planned, a taken name, a
// provisioned target, the catalogue's source. A draft names its provider only: the mock never
// keeps a token, and a handler keeps no more of one than the verdict it reads off it at once.
import { nameProblem, tokenProblem } from '../../screens/targets/rules';
import type { ActivePointerChange } from '../generated/ActivePointerChange';
import type { CatalogueSourceArg } from '../generated/CatalogueSourceArg';
import type { CliDefaultPointer } from '../generated/CliDefaultPointer';
import type { CliDefaultTarget } from '../generated/CliDefaultTarget';
import { CORE_ERROR_CODES } from '../generated/core-errors';
import type { DoctorReport } from '../generated/DoctorReport';
import type { DraftId } from '../generated/DraftId';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import UI_ERRORS from '../generated/fixtures/ui-errors.json';
import type { MachineSet } from '../generated/MachineSet';
import type { OpEvent } from '../generated/OpEvent';
import type { PlannedChange } from '../generated/PlannedChange';
import type { RegionLatency } from '../generated/RegionLatency';
import type { SshKeyInfo } from '../generated/SshKeyInfo';
import type { JsonValue } from '../generated/serde_json/JsonValue';
import type { TargetAddArgs } from '../generated/TargetAddArgs';
import type { TargetAdded } from '../generated/TargetAdded';
import type { TargetListReport } from '../generated/TargetListReport';
import type { TargetRemoved } from '../generated/TargetRemoved';
import type { TargetRenamed } from '../generated/TargetRenamed';
import type { TargetRenewed } from '../generated/TargetRenewed';
import type { TargetReport } from '../generated/TargetReport';
import type { TargetSummary } from '../generated/TargetSummary';
import type { TargetUsed } from '../generated/TargetUsed';
import type { TokenVerified } from '../generated/TokenVerified';
import { SUPPORTED_PROVIDERS, TIERS } from '../generated/target';
import type { UiError } from '../generated/UiError';
import type { UnreadableTarget } from '../generated/UnreadableTarget';
import type { Verification } from '../generated/Verification';
import type { WhoamiReport } from '../generated/WhoamiReport';
import {
  MOCK_CATALOGUE,
  MOCK_CLI_DEFAULT,
  MOCK_DOCTOR,
  MOCK_LATENCIES,
  MOCK_NOT_KEYS,
  MOCK_REPORTS,
  MOCK_SSH_KEYS,
  MOCK_TOOLCHAIN,
  MOCK_UNREADABLE,
  targetFiles,
} from './fixtures';
import type { Handler, MockOps, MockResult } from './ops';

/** The mock's target store. */
export interface MockStore {
  readonly reports: Map<string, TargetReport>;
  unreadable: UnreadableTarget[];
  cliDefault: string | null;
  /** Drafts by id: the provider only — the mock never keeps a token. */
  readonly drafts: Map<number, string>;
  nextDraft: number;
}

/** A fresh store: fixtures.ts's three readable targets, the unreadable one, prod-eu the default. */
export function mockStore(): MockStore {
  return {
    reports: new Map(MOCK_REPORTS.map((report) => [report.name, structuredClone(report)])),
    unreadable: MOCK_UNREADABLE.map((target) => structuredClone(target)),
    cliDefault: MOCK_CLI_DEFAULT,
    drafts: new Map(),
    nextDraft: 0,
  };
}

/** How long the demo's provider takes to answer a token check. */
const VERIFY_MS = 182;

/** Rust's machine::DEFAULT_REGION: what a server type is checked in when no region is given. */
const DEFAULT_REGION = 'nbg1';

const error = (code: string, message: string, fields: UiError['fields'] = {}): UiError => ({
  code,
  message,
  help: null,
  causes: [],
  fields,
});

/** Every target in the store, unreadable ones included, sorted: the names Rust lists. */
const namesIn = (store: MockStore) =>
  [...store.reports.keys(), ...store.unreadable.map((target) => target.name)].sort();

const notFound = (store: MockStore, name: string) => {
  const available = namesIn(store);
  return error(
    CORE_ERROR_CODES.TARGET_NOT_FOUND,
    `target \`${name}\` not found (available: ${available.join(', ')})`,
    { name, available },
  );
};

const draftNotFound = (draftId: DraftId): UiError => ({
  ...error(DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND, `no verified token waits as draft ${draftId}`, {
    draftId,
  }),
  help: 'It was used, discarded, or dropped when AppRafter locked; verify the token again.',
});

const invalidName = (name: string, problem: string) =>
  error(CORE_ERROR_CODES.TARGET_INVALID_NAME, `invalid target name \`${name}\`: ${problem}`, {
    name,
    problem,
  });

const invalidToken = (problem: string) =>
  error(CORE_ERROR_CODES.TARGET_INVALID_TOKEN, `invalid Hetzner Cloud token: ${problem}`, {
    problem,
  });

/**
 * The demo's 401 (a well-formed token starting with `x`), as the core projects a ping's 401:
 * the generated fixture, not a copy that could drift from what Rust sends.
 */
const TOKEN_REJECTED = UI_ERRORS.tokenRejected as UiError;

const exists = (name: string) =>
  error(CORE_ERROR_CODES.TARGET_EXISTS, `target \`${name}\` already exists`, { name });

const nothingToChange = (name: string) =>
  error(
    CORE_ERROR_CODES.TARGET_RENEW_NOTHING_TO_CHANGE,
    `renewing \`${name}\` would change nothing: no new token and no new SSH key`,
    { name },
  );

const unknownProvider = (provider: string) =>
  error(
    CORE_ERROR_CODES.TARGET_UNKNOWN_PROVIDER,
    `provider \`${provider}\` is not supported (supported: ${SUPPORTED_PROVIDERS.join(', ')})`,
    { provider, supported: [...SUPPORTED_PROVIDERS] },
  );

const change = (
  kind: string,
  object: string,
  action: PlannedChange['action'],
  detail: string | null = null,
): PlannedChange => ({ kind, object, action, detail });

const stage = (index: number, total: number, title: string): OpEvent => ({
  kind: 'stage',
  index,
  total,
  title,
});

/**
 * Rust's refusal of `op_start_machine_catalogue` at the command, before any read starts
 * (`shell.drafts.get(draft_id)?` / `TargetRef::named(..)?`): an unknown draft is
 * `DRAFT_NOT_FOUND` with `fields.draftId`, an unknown target `TARGET_NOT_FOUND` with
 * `{ name, available }`; `null` when the source exists. The one copy of the rule: D.3d's
 * catalogue handler and D.3e's richer one both call it.
 */
export function catalogueSourceRefusal(
  source: CatalogueSourceArg,
  store: MockStore,
): UiError | null {
  if (source.kind === 'draft') {
    return store.drafts.has(source.draftId) ? null : draftNotFound(source.draftId);
  }
  return namesIn(store).includes(source.name) ? null : notFound(store, source.name);
}

/**
 * What the file at `path` (a candidate's path or its `~` form) holds, as `ssh_key_inspect`
 * reports it: a public key in `~/.ssh`, a file that is not one (MOCK_NOT_KEYS), or nothing.
 */
function inspectKey(path: string): SshKeyInfo {
  const key = MOCK_SSH_KEYS.find((k) => k.path === path || k.display === path);
  if (key !== undefined) {
    const problem = key.algo === null ? 'not_public_key' : null;
    return { path: key.path, display: key.display, exists: true, algo: key.algo, problem };
  }
  const other = MOCK_NOT_KEYS.find((k) => k.path === path || k.display === path);
  return other ?? { path, display: path, exists: false, algo: null, problem: 'missing' };
}

/**
 * The key at `path` for a plan, as the core's `check_readable` takes it: a public key, or Rust's
 * refusal — `SshKeyUnreadable` for no file, `SshKeyNotPublic` for one that is not a public key
 * (GOTCHA-149: a private key by name).
 */
function keyAt(path: string): SshKeyInfo {
  const key = inspectKey(path);
  if (key.problem === 'missing') {
    throw error(
      CORE_ERROR_CODES.TARGET_SSH_KEY_UNREADABLE,
      `SSH key path \`${path}\` does not exist`,
      {
        path,
        problem: 'missing',
      },
    );
  }
  if (key.problem !== null) {
    const origin = `SSH key \`${key.path}\``;
    const privateKey = key.problem === 'private_key';
    throw error(
      CORE_ERROR_CODES.TARGET_SSH_KEY_NOT_PUBLIC,
      privateKey
        ? `${origin} is a private key: AppRafter never sends a private key to the provider`
        : `${origin} is not an OpenSSH public key`,
      { origin, privateKey },
    );
  }
  return key;
}

/** As a JSON result of a mock operation. */
const result = (value: unknown): MockResult => ({ result: value as JsonValue });

/** The D.3 commands' answers, over `ops` (reads and plans) and `store`. */
export function targetHandlers(ops: MockOps, store: MockStore): Record<string, Handler> {
  /** The readable target `name`, or Rust's refusal: not found, or what made it unreadable. */
  const named = (name: string): TargetReport => {
    const report = store.reports.get(name);
    if (report !== undefined) return { ...report, isCliDefault: name === store.cliDefault };
    const unreadable = store.unreadable.find((target) => target.name === name);
    throw unreadable === undefined ? notFound(store, name) : unreadable.error;
  };

  /** A handler whose thrown UiError is its rejection, as a Rust command's `?`. */
  const refusing =
    (handler: Handler): Handler =>
    (args) => {
      try {
        return handler(args);
      } catch (refusal) {
        return Promise.reject(refusal);
      }
    };

  const summaryOf = (report: TargetReport): TargetSummary => ({
    name: report.name,
    provider: report.provider,
    region: report.region,
    serverType: report.serverType,
    defaultTier: report.defaultTier,
    tierLevel: report.tierLevel,
    isCliDefault: report.name === store.cliDefault,
  });

  const pointer = (): CliDefaultPointer => {
    if (store.cliDefault === null) return { status: 'unset' };
    return namesIn(store).includes(store.cliDefault)
      ? { status: 'set', name: store.cliDefault }
      : { status: 'missing', name: store.cliDefault };
  };

  const provisionedServer = (report: TargetReport) =>
    report.provisioned.status === 'provisioned' ? report.provisioned.server : null;

  /** whoami's answer, its verification `verification(report)` for a found default. */
  const whoamiOf = (verification: (report: TargetReport) => Verification): WhoamiReport => {
    const identity = 'anonymous_self_hosted';
    const name = store.cliDefault;
    if (name === null) return { identity, cliDefault: { status: 'none' } };
    const report = store.reports.get(name);
    let cliDefault: CliDefaultTarget;
    if (report === undefined) {
      cliDefault = { status: 'missing', name, available: namesIn(store) };
    } else {
      cliDefault = {
        status: 'found',
        target: {
          name,
          provider: report.provider,
          verification: verification(report),
          region: report.region,
          serverType: report.serverType,
          defaultTier: report.defaultTier,
          clusterName: report.clusterName,
          sshKey: report.sshKey,
        },
      };
    }
    return { identity, cliDefault };
  };

  /** The default moves off `name` (removed): to the first other name, or nowhere. */
  const nextDefault = (name: string) => namesIn(store).find((other) => other !== name) ?? null;

  return {
    target_list: (): TargetListReport => ({
      targets: [...store.reports.values()]
        .sort((a, b) => a.name.localeCompare(b.name))
        .map(summaryOf),
      unreadable: [...store.unreadable],
      cliDefault: pointer(),
    }),

    target_show: refusing((args) => named((args as { name: string }).name)),

    ssh_key_candidates: () => [...MOCK_SSH_KEYS],

    ssh_key_inspect: (args): SshKeyInfo => inspectKey((args as { path: string }).path),

    toolchain_status: () => structuredClone(MOCK_TOOLCHAIN),

    whoami: () => whoamiOf(() => ({ status: 'skipped', reason: 'no_ping' })),

    op_start_verify_token: (args) => {
      const { provider, token } = args as { provider: string; token: string };
      // Read off the token at once, in Rust's order (provider, format, the provider's answer):
      // the read keeps the verdict, never the token.
      const problem = tokenProblem(token);
      let verdict: UiError | null = null;
      if (!(SUPPORTED_PROVIDERS as readonly string[]).includes(provider)) {
        verdict = unknownProvider(provider);
      } else if (problem !== null) {
        verdict = invalidToken(problem);
      } else if (token.startsWith('x')) {
        verdict = TOKEN_REJECTED;
      }
      return ops.startRead(`Verify the ${provider} token`, null, {
        end: () => {
          if (verdict !== null) return { error: verdict };
          store.nextDraft += 1;
          store.drafts.set(store.nextDraft, provider);
          return result({
            draftId: store.nextDraft,
            elapsedMs: VERIFY_MS,
          } satisfies TokenVerified);
        },
      });
    },

    op_start_machine_catalogue: (args) => {
      const { source } = args as { source: CatalogueSourceArg };
      const refusal = catalogueSourceRefusal(source, store);
      if (refusal !== null) return Promise.reject(refusal);
      const [title, target] =
        source.kind === 'target'
          ? [`Machine catalogue · ${source.name}`, source.name]
          : ['Machine catalogue', null];
      return ops.startRead(title, target, { end: () => result(structuredClone(MOCK_CATALOGUE)) });
    },

    op_start_region_latencies: (args) => {
      const { regions } = args as { regions: string[] };
      return ops.startRead('Region latency', null, {
        end: () =>
          result(
            regions.map(
              (region): RegionLatency => ({ region, latencyMs: MOCK_LATENCIES[region] ?? null }),
            ),
          ),
      });
    },

    op_start_doctor: (args) => {
      const { target } = args as { target: string };
      const found = namesIn(store).includes(target);
      const total = found ? 3 : 2;
      const [, clusterGroup, computerGroup] = MOCK_DOCTOR.groups;
      const report: DoctorReport = found
        ? {
            target,
            groups: MOCK_DOCTOR.groups.map((group) =>
              group === clusterGroup
                ? {
                    ...group,
                    checks: group.checks.map((check) => ({
                      ...check,
                      fix: { kind: 'fetch_kubeconfig', target } as const,
                    })),
                  }
                : group,
            ),
          }
        : {
            target,
            groups: [
              {
                id: 'target',
                checks: [
                  {
                    id: 'target_exists',
                    tool: null,
                    status: 'fail',
                    title: `Target \`${target}\` exists`,
                    detail: null,
                    fix: { kind: 'add_target', name: target, available: namesIn(store) },
                  },
                ],
              },
              ...(computerGroup === undefined ? [] : [computerGroup]),
            ],
          };
      const events = found
        ? [stage(1, total, 'Target'), stage(2, total, 'Cluster'), stage(3, total, 'This computer')]
        : [stage(1, total, 'Target'), stage(2, total, 'This computer')];
      return ops.startRead(`Doctor · ${target}`, target, {
        events,
        end: () => result(report),
      });
    },

    op_start_whoami: () =>
      ops.startRead("Verify the CLI default's token", null, {
        end: () =>
          result(
            whoamiOf(
              (report): Verification =>
                report.token.set
                  ? { status: 'verified', elapsedMs: VERIFY_MS }
                  : { status: 'skipped', reason: 'no_token' },
            ),
          ),
      }),

    op_plan_target_add: refusing((args) => {
      const { name, provider, draftId, sshKey, region, tier, serverType } = (
        args as { args: TargetAddArgs }
      ).args;
      const drafted = store.drafts.get(draftId);
      if (drafted === undefined) throw draftNotFound(draftId);
      if (drafted !== provider) {
        throw error(
          DESKTOP_ERROR_CODES.INTERNAL,
          `internal error: draft ${draftId} was verified for ${drafted}, not ${provider}`,
        );
      }
      const problem = nameProblem(name);
      if (problem !== null) throw invalidName(name, problem);
      const key = sshKey === null ? null : keyAt(sshKey);
      if (namesIn(store).includes(name)) throw exists(name);
      // Planned: the plan holds the token now (overview §3.12.1).
      store.drafts.delete(draftId);
      const becomesDefault = store.cliDefault === null;
      const detail = [
        `provider ${provider}`,
        ...(region === null ? [] : [`region ${region}`]),
        ...(tier === null ? [] : [`tier ${tier}`]),
        ...(serverType === null ? [] : [`server type ${serverType}`]),
        ...(sshKey === null ? [] : [`ssh key ${sshKey}`]),
      ].join(', ');
      const changes = [
        change('Target', name, 'create', detail),
        change('Credentials', name, 'create'),
        ...(becomesDefault ? [change('CliDefault', name, 'set_default', `none → ${name}`)] : []),
      ];
      return ops.registerPlan(
        { class: 'bounded', title: `Add target ${name}`, changes, target: name },
        {
          end: () => {
            if (namesIn(store).includes(name)) return { error: exists(name) };
            store.reports.set(name, {
              name,
              isCliDefault: becomesDefault,
              provider,
              region,
              serverType,
              defaultTier: tier,
              tierLevel: TIERS.find((t) => t.id === tier)?.level ?? null,
              clusterName: null,
              sshKey: key,
              token: { set: true, chars: 64 },
              ...targetFiles(name),
              provisioned: { status: 'not_provisioned' },
            });
            if (becomesDefault) store.cliDefault = name;
            return result({
              name,
              replaced: false,
              isCliDefault: becomesDefault,
              cliDefault: becomesDefault ? { from: null, to: name } : null,
              token: { status: 'verified', elapsedMs: VERIFY_MS },
              sku:
                serverType === null
                  ? null
                  : {
                      status: 'validated',
                      sku: serverType,
                      region: region ?? DEFAULT_REGION,
                      regionWasDefault: region === null,
                    },
            } satisfies TargetAdded);
          },
        },
      );
    }),

    // As the core's plan_renew: a token, a key, or both, each a change only when it differs
    // from what is stored (the mock keeps no token, so any token given counts as new); nothing
    // to change is refused, at the plan and again when it runs.
    op_plan_target_renew: refusing((args) => {
      const { name, token, sshKey } = args as {
        name: string;
        token: string | null;
        sshKey?: string | null;
      };
      const report = named(name);
      if (token !== null) {
        const problem = tokenProblem(token);
        if (problem !== null) throw invalidToken(problem);
      }
      // The token is checked with the provider when the plan runs; only the verdict is kept.
      const rejected = token?.startsWith('x') ?? false;
      const rotates = token !== null;
      const key = sshKey === undefined || sshKey === null ? null : keyAt(sshKey);
      // A key path the plan changes: a new one; the one stored now is left out, as the core does.
      const newKey = key !== null && key.path !== report.sshKey?.path ? key : null;
      if (!rotates && newKey === null) throw nothingToChange(name);
      const changes = [
        ...(rotates ? [change('Credentials', name, 'replace', 'API token')] : []),
        ...(newKey === null
          ? []
          : [
              change(
                'Target',
                name,
                'update',
                `ssh key: ${report.sshKey?.display ?? 'not set'} → ${newKey.display}`,
              ),
            ]),
      ];
      const title = !rotates
        ? `Change the SSH key of ${name}`
        : newKey === null
          ? `Rotate the API token of ${name}`
          : `Rotate the API token and change the SSH key of ${name}`;
      return ops.registerPlan(
        { class: 'bounded', title, changes, target: name },
        {
          end: () => {
            if (rejected) return { error: TOKEN_REJECTED };
            const current = store.reports.get(name);
            if (current === undefined) return { error: notFound(store, name) };
            const keyChanged = newKey !== null && newKey.path !== current.sshKey?.path;
            if (!rotates && !keyChanged) return { error: nothingToChange(name) };
            store.reports.set(name, {
              ...current,
              ...(rotates && { token: { set: true, chars: 64 } }),
              ...(keyChanged && { sshKey: newKey }),
            });
            return result({
              name,
              token: rotates ? { status: 'verified', elapsedMs: VERIFY_MS } : null,
              sshKeyChanged: keyChanged,
            } satisfies TargetRenewed);
          },
        },
      );
    }),

    op_plan_target_use: refusing((args) => {
      const { name } = args as { name: string };
      named(name);
      const current = store.cliDefault;
      const changes =
        current === name
          ? []
          : [change('CliDefault', name, 'set_default', `${current ?? 'none'} → ${name}`)];
      return ops.registerPlan(
        { class: 'reversible', title: `Make ${name} the CLI default`, changes, target: name },
        {
          end: () => {
            const from = store.cliDefault;
            let moved: ActivePointerChange | null = null;
            if (from !== name) {
              store.cliDefault = name;
              moved = { from, to: name };
            }
            return result({ name, pointer: moved } satisfies TargetUsed);
          },
        },
      );
    }),

    op_plan_target_rename: refusing((args) => {
      const { from, to } = args as { from: string; to: string };
      const report = named(from);
      const problem = nameProblem(to);
      if (problem !== null) throw invalidName(to, problem);
      if (from === to) {
        throw error(
          CORE_ERROR_CODES.TARGET_SAME_NAME,
          `source and destination target names are both \`${to}\` — nothing to rename`,
          { name: to },
        );
      }
      if (namesIn(store).includes(to)) throw exists(to);
      const arrow = `${from} → ${to}`;
      const hasState = provisionedServer(report) !== null;
      const wasDefault = store.cliDefault === from;
      const changes = [
        change('Target', from, 'rename', arrow),
        ...(hasState ? [change('LocalState', from, 'rename', arrow)] : []),
        ...(wasDefault ? [change('CliDefault', to, 'set_default', arrow)] : []),
      ];
      return ops.registerPlan(
        { class: 'bounded', title: `Rename target ${from} to ${to}`, changes, target: from },
        {
          end: () => {
            const current = store.reports.get(from);
            if (current === undefined) return { error: notFound(store, from) };
            if (namesIn(store).includes(to)) return { error: exists(to) };
            store.reports.delete(from);
            store.reports.set(to, { ...current, name: to, ...targetFiles(to) });
            const movedDefault = store.cliDefault === from;
            if (movedDefault) store.cliDefault = to;
            return result({
              from,
              to,
              stateMoved: provisionedServer(current) !== null,
              cliDefault: movedDefault ? { from, to } : null,
            } satisfies TargetRenamed);
          },
        },
      );
    }),

    op_plan_target_remove: refusing((args) => {
      const { name } = args as { name: string };
      const report = named(name);
      const server = provisionedServer(report);
      const next = nextDefault(name);
      const changes = [
        change('Target', name, 'delete'),
        change('Credentials', name, 'delete'),
        ...(server === null
          ? []
          : [
              change(
                'LocalState',
                name,
                'delete',
                `records server ${server.serverName} (id ${server.serverId}); the server keeps running at the provider`,
              ),
            ]),
        ...(store.cliDefault === name
          ? [
              next === null
                ? change('CliDefault', name, 'clear_default')
                : change('CliDefault', next, 'set_default', `${name} → ${next}`),
            ]
          : []),
      ];
      return ops.registerPlan(
        {
          class: 'destructive',
          title: `Remove target ${name} from this computer`,
          changes,
          target: name,
        },
        {
          end: () => {
            const current = store.reports.get(name);
            if (current === undefined) return { error: notFound(store, name) };
            store.reports.delete(name);
            const orphanedServer = provisionedServer(current);
            let cliDefault: ActivePointerChange | null = null;
            if (store.cliDefault === name) {
              const to = nextDefault(name);
              store.cliDefault = to;
              cliDefault = { from: name, to };
            }
            return result({
              name,
              stateRemoved: orphanedServer !== null,
              orphanedServer,
              cliDefault,
            } satisfies TargetRemoved);
          },
        },
      );
    }),

    op_plan_target_machine: refusing((args) => {
      const { name, sku, region } = args as { name: string; sku: string; region: string | null };
      const report = named(name);
      const server = provisionedServer(report);
      if (server !== null) {
        throw error(
          CORE_ERROR_CODES.TARGET_PROVISIONED,
          `target \`${name}\` has a provisioned server (\`${server.serverName}\`, id ${server.serverId}), so its machine or region cannot change`,
          { name, serverId: server.serverId, serverName: server.serverName },
        );
      }
      const changes = [
        change('Target', name, 'update', `server type: ${report.serverType ?? 'not set'} → ${sku}`),
        ...(region !== null && region !== report.region
          ? [change('Target', name, 'update', `region: ${report.region ?? 'not set'} → ${region}`)]
          : []),
      ];
      return ops.registerPlan(
        { class: 'bounded', title: `Set the machine of ${name}`, changes, target: name },
        {
          end: () => {
            const current = store.reports.get(name);
            if (current === undefined) return { error: notFound(store, name) };
            const where = region ?? current.region;
            store.reports.set(name, { ...current, serverType: sku, region: where });
            return result({
              name,
              sku,
              region: where,
              skuCheck: {
                status: 'validated',
                sku,
                region: where ?? DEFAULT_REGION,
                regionWasDefault: where === null,
              },
            } satisfies MachineSet);
          },
        },
      );
    }),

    target_draft_discard: (args) => {
      store.drafts.delete((args as { draftId: DraftId }).draftId);
      return null;
    },
  };
}
