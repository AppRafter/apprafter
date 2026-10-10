// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The D.3 commands on the mock IPC, through api.ts and the real operations store: they answer as
// Rust does — the store, the drafts a verify leaves and the add plan takes, the refusals.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { settleIpc } from '../../test/settle';
import * as api from '../api';
import { CORE_ERROR_CODES } from '../generated/core-errors';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import type { OpId } from '../generated/OpId';
import type { PlanView } from '../generated/PlanView';
import { HETZNER_TOKEN_LEN } from '../generated/target';
import type { UiError } from '../generated/UiError';
import {
  attach,
  execute,
  type OpEnd,
  operationsSnapshot,
  resetOperations,
  watchOperations,
} from '../operations';
import { MOCK_CATALOGUE } from './flows';
import { installMockIpc } from './index';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  await settleIpc();
  resetOperations();
  clearMocks();
});

function ended(opId: OpId): Promise<OpEnd> {
  return new Promise((resolve) => {
    let off = () => {};
    const check = () => {
      const end = operationsSnapshot().get(opId)?.end ?? null;
      if (end !== null) {
        off();
        resolve(end);
      }
    };
    off = watchOperations(check);
    check();
  });
}
async function runPlan(view: PlanView): Promise<OpEnd> {
  const release = await execute(view.opId);
  try {
    return await ended(view.opId);
  } finally {
    release();
  }
}
async function runRead(opId: OpId): Promise<OpEnd> {
  const release = attach(opId);
  try {
    return await ended(opId);
  } finally {
    release();
  }
}
const result = (end: OpEnd) =>
  end.state === 'finished' && end.outcome.status === 'completed' ? end.outcome.result : end;
const failure = (end: OpEnd) => (end.state === 'failed' ? end.error : null);
const refusal = (x: unknown) => (x as api.IpcError).error;
/** What the call was refused with; a call that answers fails the test. */
async function refusalOf(call: Promise<unknown>): Promise<UiError> {
  try {
    await call;
  } catch (x) {
    return refusal(x);
  }
  throw new Error('the call was answered, not refused');
}
/** Token-shaped, nobody's: built here, never written out. */
const goodToken = () => 'k'.repeat(HETZNER_TOKEN_LEN);

test('the list: sorted, the CLI default marked, the unreadable one apart', async () => {
  const list = await api.targetList();
  expect(list.targets.map((t) => t.name)).toEqual(['lab', 'prod-eu', 'staging']);
  expect(list.targets.filter((t) => t.isCliDefault).map((t) => t.name)).toEqual(['prod-eu']);
  expect(list.cliDefault).toEqual({ status: 'set', name: 'prod-eu' });
  expect(list.unreadable.map((u) => u.name)).toEqual(['broken']);
});

test('show: a report by name; an unknown name is TARGET_NOT_FOUND with the names there are', async () => {
  expect((await api.targetShow('prod-eu')).provisioned).toMatchObject({
    status: 'provisioned',
    server: { serverId: 4711 },
  });
  const e = await refusalOf(api.targetShow('ghost'));
  expect(e.code).toBe(CORE_ERROR_CODES.TARGET_NOT_FOUND);
  expect(e.fields).toMatchObject({
    name: 'ghost',
    available: ['broken', 'lab', 'prod-eu', 'staging'],
  });
  // An unreadable target exists: showing it is what made it unreadable.
  expect((await refusalOf(api.targetShow('broken'))).code).toBe(
    'apprafter::target::invalid_config',
  );
});

test('a rename of the CLI default moves the pointer; the old name is then not found', async () => {
  const end = await runPlan(await api.opPlanTargetRename('prod-eu', 'prod-us'));
  expect(result(end)).toMatchObject({
    from: 'prod-eu',
    to: 'prod-us',
    stateMoved: true,
    cliDefault: { from: 'prod-eu', to: 'prod-us' },
  });
  expect((await api.targetList()).cliDefault).toEqual({ status: 'set', name: 'prod-us' });
  // Its files moved with it, as the core's rename moves the target's directory.
  expect(await api.targetShow('prod-us')).toMatchObject({
    name: 'prod-us',
    configFile: '~/.config/apprafter/targets/prod-us/config.yaml',
    credentialsFile: '~/.config/apprafter/targets/prod-us/credentials.yaml',
  });
  const gone = await refusalOf(api.targetShow('prod-eu'));
  expect(gone.code).toBe(CORE_ERROR_CODES.TARGET_NOT_FOUND);
});

test("a rename's refusals: a bad name, the same name, a taken one", async () => {
  const bad = await refusalOf(api.opPlanTargetRename('lab', 'Lab 2'));
  expect(bad).toMatchObject({ code: CORE_ERROR_CODES.TARGET_INVALID_NAME });
  expect(typeof bad.fields.problem).toBe('string');
  const same = await refusalOf(api.opPlanTargetRename('lab', 'lab'));
  expect(same.code).toBe(CORE_ERROR_CODES.TARGET_SAME_NAME);
  const taken = await refusalOf(api.opPlanTargetRename('lab', 'staging'));
  expect(taken.code).toBe(CORE_ERROR_CODES.TARGET_EXISTS);
});

test('removing prod-eu names the server it leaves running and repoints as Rust does', async () => {
  const view = await api.opPlanTargetRemove('prod-eu');
  expect(view.class).toBe('destructive');
  // Rust repoints to the alphabetically first remaining target that can be read, passing over
  // broken (WI-458: target::remove's next_default); its plan line is the one
  // the_mock_store_default_moves_past_broken_to_lab asserts in apprafter-core.
  expect(view.changes.find((c) => c.kind === 'CliDefault')).toEqual({
    kind: 'CliDefault',
    object: 'lab',
    action: 'set_default',
    detail: 'prod-eu → lab, passing over broken, which cannot be read',
  });
  expect(result(await runPlan(view))).toMatchObject({
    name: 'prod-eu',
    orphanedServer: { serverId: 4711, serverName: 'prod-eu-1' },
    cliDefault: { from: 'prod-eu', to: 'lab' },
    skippedUnreadable: ['broken'],
  });
  expect((await api.targetList()).targets.map((t) => t.name)).toEqual(['lab', 'staging']);
});

test('an unreadable target is removed through the same destructive plan, naming its file', async () => {
  const view = await api.opPlanTargetRemove('broken');
  expect(view.class).toBe('destructive');
  expect(view.changes).toEqual([
    {
      kind: 'Target',
      object: 'broken',
      action: 'delete',
      detail: 'config.yaml cannot be read: expected a mapping',
    },
    { kind: 'Credentials', object: 'broken', action: 'delete', detail: null },
  ]);
  expect(result(await runPlan(view))).toEqual({
    name: 'broken',
    stateRemoved: false,
    orphanedServer: null,
    cliDefault: null,
    skippedUnreadable: [],
  });
  const list = await api.targetList();
  expect(list.unreadable).toEqual([]);
  expect(list.targets.map((t) => t.name)).toEqual(['lab', 'prod-eu', 'staging']);
});

test('with only unreadable targets left the default is cleared, and the line says why', async () => {
  for (const name of ['lab', 'staging']) await runPlan(await api.opPlanTargetRemove(name));
  const view = await api.opPlanTargetRemove('prod-eu');
  expect(view.changes.at(-1)).toEqual({
    kind: 'CliDefault',
    object: 'prod-eu',
    action: 'clear_default',
    detail: 'no readable target left: broken cannot be read',
  });
  expect(result(await runPlan(view))).toMatchObject({
    cliDefault: { from: 'prod-eu', to: null },
    skippedUnreadable: ['broken'],
  });
  expect((await api.targetList()).cliDefault).toEqual({ status: 'unset' });
});

test('use is reversible and moves the pointer; on the default it changes nothing', async () => {
  const view = await api.opPlanTargetUse('lab');
  expect(view.class).toBe('reversible');
  expect(result(await runPlan(view))).toEqual({
    name: 'lab',
    pointer: { from: 'prod-eu', to: 'lab' },
  });
  const again = await api.opPlanTargetUse('lab');
  expect(again.changes).toEqual([]);
  expect(result(await runPlan(again))).toEqual({ name: 'lab', pointer: null });
});

test('the machine of a provisioned target is refused; of another it is planned and set', async () => {
  const refused = await refusalOf(api.opPlanTargetMachine('prod-eu', 'cx32', null));
  expect(refused).toMatchObject({
    code: CORE_ERROR_CODES.TARGET_PROVISIONED,
    fields: { name: 'prod-eu', serverId: 4711, serverName: 'prod-eu-1' },
  });
  const view = await api.opPlanTargetMachine('lab', 'cx32', 'nbg1');
  expect(view.class).toBe('bounded');
  expect(view.changes.map((c) => c.detail)).toEqual([
    'server type: not set → cx32',
    'region: hel1 → nbg1',
  ]);
  expect(result(await runPlan(view))).toMatchObject({ name: 'lab', sku: 'cx32', region: 'nbg1' });
  expect((await api.targetShow('lab')).serverType).toBe('cx32');
});

test('verify: a malformed token, the x-token 401, and a good one that becomes a draft', async () => {
  const good = goodToken();
  const malformed = failure(
    await runRead(await api.opStartVerifyToken('hetzner-cloud', good.slice(1))),
  );
  expect(malformed?.code).toBe(CORE_ERROR_CODES.TARGET_INVALID_TOKEN);
  const rejected = failure(
    await runRead(await api.opStartVerifyToken('hetzner-cloud', 'x'.repeat(HETZNER_TOKEN_LEN))),
  );
  expect(rejected?.code).toBe(CORE_ERROR_CODES.TARGET_TOKEN_REJECTED);
  expect(rejected?.fields).toMatchObject({ provider: 'hetzner-cloud', status: 401 });
  const verified = result(await runRead(await api.opStartVerifyToken('hetzner-cloud', good))) as {
    draftId: number;
    elapsedMs: number;
  };
  expect(Object.keys(verified).sort()).toEqual(['draftId', 'elapsedMs']);
  expect(verified.elapsedMs).toBe(182);
  expect(typeof verified.draftId).toBe('number');
});

test('add takes the draft when planning succeeds; a second plan with it is DRAFT_NOT_FOUND', async () => {
  const { draftId } = result(
    await runRead(await api.opStartVerifyToken('hetzner-cloud', goodToken())),
  ) as { draftId: number };
  const args = {
    name: 'lab-2',
    provider: 'hetzner-cloud',
    draftId,
    sshKey: null,
    region: 'nbg1',
    tier: 'solo',
    serverType: 'cx22',
  };
  // A refused plan takes nothing: the corrected form uses the same draft.
  const taken = await refusalOf(api.opPlanTargetAdd({ ...args, name: 'staging' }));
  expect(taken.code).toBe(CORE_ERROR_CODES.TARGET_EXISTS);
  const view = await api.opPlanTargetAdd(args);
  expect(view.class).toBe('bounded');
  const again = await refusalOf(api.opPlanTargetAdd(args));
  expect(again.code).toBe(DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND);
  expect(again.fields).toEqual({ draftId });
  expect(result(await runPlan(view))).toMatchObject({ name: 'lab-2', cliDefault: null });
  expect((await api.targetList()).targets.map((t) => t.name)).toContain('lab-2');
  expect((await api.targetShow('lab-2')).tierLevel).toBe(1);
});

test('add with an SSH key: an unreadable one is refused and keeps the draft; a found one is saved', async () => {
  const { draftId } = result(
    await runRead(await api.opStartVerifyToken('hetzner-cloud', goodToken())),
  ) as { draftId: number };
  const args = {
    name: 'lab-2',
    provider: 'hetzner-cloud',
    draftId,
    sshKey: '~/.ssh/nothing.pub',
    region: null,
    tier: null,
    serverType: null,
  };
  const unreadable = await refusalOf(api.opPlanTargetAdd(args));
  expect(unreadable.code).toBe(CORE_ERROR_CODES.TARGET_SSH_KEY_UNREADABLE);
  const view = await api.opPlanTargetAdd({ ...args, sshKey: '~/.ssh/work.pub' });
  await runPlan(view);
  expect((await api.targetShow('lab-2')).sshKey).toEqual({
    path: '/home/alex/.ssh/work.pub',
    display: '~/.ssh/work.pub',
    exists: true,
    algo: 'ssh-rsa',
    problem: null,
  });
});

test('the catalogue of an unknown draft or target is refused at the command, before a read starts (as Rust)', async () => {
  const refused = (source: Parameters<typeof api.opStartMachineCatalogue>[0]) =>
    api.opStartMachineCatalogue(source).then(
      () => null,
      (x: unknown) => refusal(x).code,
    );
  expect(await refused({ kind: 'draft', draftId: 999 })).toBe(DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND);
  expect(await refused({ kind: 'target', name: 'ghost' })).toBe(CORE_ERROR_CODES.TARGET_NOT_FOUND);
  expect(await api.opList()).toEqual([]);
  const read = await api.opStartMachineCatalogue({ kind: 'target', name: 'prod-eu' });
  // The catalogue installMockIpc answers with is flows.ts's (D.3e deviation 5).
  expect(result(await runRead(read))).toEqual(MOCK_CATALOGUE);
  expect((await api.opList())[0]).toMatchObject({
    title: 'Machine catalogue · prod-eu',
    target: 'prod-eu',
  });
});

test('a lock drops every draft, as Rust does', async () => {
  const { draftId } = result(
    await runRead(await api.opStartVerifyToken('hetzner-cloud', goodToken())),
  ) as { draftId: number };
  await api.lockNow();
  await api.unlock();
  const gone = await refusalOf(api.opStartMachineCatalogue({ kind: 'draft', draftId }));
  expect(gone.code).toBe(DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND);
});

test('a discarded draft is gone; an unknown one is no error', async () => {
  const { draftId } = result(
    await runRead(await api.opStartVerifyToken('hetzner-cloud', goodToken())),
  ) as { draftId: number };
  await api.targetDraftDiscard(draftId);
  await api.targetDraftDiscard(999);
  const gone = await refusalOf(api.opStartMachineCatalogue({ kind: 'draft', draftId }));
  expect(gone.code).toBe(DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND);
});

test('renew: a malformed token is refused at once; the x-token fails when the plan runs', async () => {
  const bad = await refusalOf(api.opPlanTargetRenew('lab', 'short', null));
  expect(bad.code).toBe(CORE_ERROR_CODES.TARGET_INVALID_TOKEN);
  const view = await api.opPlanTargetRenew('lab', 'x'.repeat(HETZNER_TOKEN_LEN), null);
  expect(view.class).toBe('bounded');
  expect(failure(await runPlan(view))?.code).toBe(CORE_ERROR_CODES.TARGET_TOKEN_REJECTED);
  const ok = await api.opPlanTargetRenew('lab', goodToken(), null);
  expect(result(await runPlan(ok))).toEqual({
    name: 'lab',
    token: { status: 'verified', elapsedMs: 182 },
    sshKeyChanged: false,
  });
});

test('renew with an SSH key: an unreadable key is refused; a new one is planned and saved', async () => {
  const unreadable = await refusalOf(
    api.opPlanTargetRenew('lab', goodToken(), '/home/alex/.ssh/nothing.pub'),
  );
  expect(unreadable.code).toBe(CORE_ERROR_CODES.TARGET_SSH_KEY_UNREADABLE);
  expect(unreadable.fields).toMatchObject({ path: '/home/alex/.ssh/nothing.pub' });
  const view = await api.opPlanTargetRenew('lab', goodToken(), '/home/alex/.ssh/work.pub');
  expect(view.class).toBe('bounded');
  expect(view.changes).toEqual([
    { kind: 'Credentials', object: 'lab', action: 'replace', detail: 'API token' },
    {
      kind: 'Target',
      object: 'lab',
      action: 'update',
      detail: 'ssh key: ~/.ssh/lab.pub → ~/.ssh/work.pub',
    },
  ]);
  expect(result(await runPlan(view))).toEqual({
    name: 'lab',
    token: { status: 'verified', elapsedMs: 182 },
    sshKeyChanged: true,
  });
  expect((await api.targetShow('lab')).sshKey).toEqual({
    path: '/home/alex/.ssh/work.pub',
    display: '~/.ssh/work.pub',
    exists: true,
    algo: 'ssh-rsa',
    problem: null,
  });
  // The key it has now, with another new token: only the token changes, as the core leaves an
  // equal path out.
  const same = await api.opPlanTargetRenew(
    'lab',
    'm'.repeat(HETZNER_TOKEN_LEN),
    '/home/alex/.ssh/work.pub',
  );
  expect(same.changes.map((c) => c.kind)).toEqual(['Credentials']);
});

test('a typed key path: `~/` expands against the home as Rust does, then the path alone finds the file', async () => {
  expect(await api.sshKeyInspect('~/.ssh/work.pub')).toEqual({
    path: '/home/alex/.ssh/work.pub',
    display: '~/.ssh/work.pub',
    exists: true,
    algo: 'ssh-rsa',
    problem: null,
  });
  // As Rust joins the rest one component at a time, empty ones dropped (69546a1d): a doubled
  // or trailing `/` finds the same key, and `~/` alone is the home, a directory: unreadable.
  for (const typed of ['~/.ssh//work.pub', '~/.ssh/work.pub/', '~//.ssh/work.pub']) {
    expect((await api.sshKeyInspect(typed)).path, typed).toBe('/home/alex/.ssh/work.pub');
  }
  expect(await api.sshKeyInspect('~/')).toEqual({
    path: '/home/alex',
    display: '~/',
    exists: true,
    algo: null,
    problem: 'unreadable',
  });
  const missing = await api.sshKeyInspect('~/.ssh/nothing.pub');
  expect([missing.path, missing.display]).toEqual([
    '/home/alex/.ssh/nothing.pub',
    '~/.ssh/nothing.pub',
  ]);
  // The plan saves the expanded path.
  await runPlan(await api.opPlanTargetRenew('lab', null, '~/.ssh/work.pub'));
  expect((await api.targetShow('lab')).sshKey?.path).toBe('/home/alex/.ssh/work.pub');
});

// As target_ops' typed_key_path: what is not a full path once `~/` is expanded would resolve
// against the app's working directory, so it is refused, as typed, before anything is looked at
// or planned. `~user/` is not expanded, and a `\` is a filename character on the Unix home the
// mock models (`~\` expands on Windows only).
test('a relative key path is refused by the lookup and both plans, and the draft stays', async () => {
  const relative = ['.ssh/work.pub', 'work.pub', '~', '~alex/.ssh/work.pub', '~\\.ssh\\work.pub'];
  const refused = async (typed: string, call: Promise<unknown>) => {
    const e = await refusalOf(call);
    expect(e.code, typed).toBe(DESKTOP_ERROR_CODES.RELATIVE_PATH);
    expect(e.message).toBe(`\`${typed}\` is not a full path`);
    expect(e.help).toBe('Give the full path, or start it with ~/ for a path in your home folder.');
    expect(e.fields).toEqual({ path: typed });
  };
  const { draftId } = result(
    await runRead(await api.opStartVerifyToken('hetzner-cloud', goodToken())),
  ) as { draftId: number };
  for (const typed of relative) {
    await refused(typed, api.sshKeyInspect(typed));
    await refused(typed, api.opPlanTargetRenew('lab', null, typed));
    await refused(
      typed,
      api.opPlanTargetAdd({
        name: 'lab-2',
        provider: 'hetzner-cloud',
        draftId,
        sshKey: typed,
        region: null,
        tier: null,
        serverType: null,
      }),
    );
  }
  // Before the name and the token are looked at, as Rust refuses it before planning.
  await refused('work.pub', api.opPlanTargetRenew('lab', 'short', 'work.pub'));
  // The draft stays for the corrected form.
  const view = await api.opPlanTargetAdd({
    name: 'lab-2',
    provider: 'hetzner-cloud',
    draftId,
    sshKey: '~/.ssh/work.pub',
    region: null,
    tier: null,
    serverType: null,
  });
  expect(view.class).toBe('bounded');
});

test('the home itself as the key: inspected as unreadable, refused by a plan as Rust does', async () => {
  const e = await refusalOf(api.opPlanTargetRenew('lab', null, '~/'));
  expect(e.code).toBe(CORE_ERROR_CODES.TARGET_SSH_KEY_UNREADABLE);
  expect(e.fields).toEqual({ path: '/home/alex', problem: 'unreadable' });
});

test('a file that is not a public key: inspected with no type and why; refused by the plans (GOTCHA-149)', async () => {
  expect(await api.sshKeyInspect('/home/alex/.ssh/id_ed25519')).toEqual({
    path: '/home/alex/.ssh/id_ed25519',
    display: '~/.ssh/id_ed25519',
    exists: true,
    algo: null,
    problem: 'private_key',
  });
  expect((await api.sshKeyInspect('/home/alex/notes.txt')).problem).toBe('not_public_key');
  expect((await api.sshKeyInspect('/home/alex/.ssh/nothing.pub')).problem).toBe('missing');
  const privateKey = await refusalOf(
    api.opPlanTargetRenew('lab', null, '/home/alex/.ssh/id_ed25519'),
  );
  expect(privateKey.code).toBe(CORE_ERROR_CODES.TARGET_SSH_KEY_NOT_PUBLIC);
  expect(privateKey.message).toBe(
    'SSH key `/home/alex/.ssh/id_ed25519` is a private key: AppRafter never sends a private key to the provider',
  );
  expect(privateKey.fields).toEqual({
    origin: 'SSH key `/home/alex/.ssh/id_ed25519`',
    privateKey: true,
  });
  const note = await refusalOf(api.opPlanTargetRenew('lab', null, '/home/alex/notes.txt'));
  expect(note.fields).toMatchObject({ privateKey: false });
  expect((await api.targetShow('lab')).sshKey?.path).toBe('/home/alex/.ssh/lab.pub');
});

test('renew with a key and no token: only the key is planned and saved, the token kept', async () => {
  const before = (await api.targetShow('lab')).token;
  const view = await api.opPlanTargetRenew('lab', null, '/home/alex/.ssh/work.pub');
  expect(view.title).toBe('Change the SSH key of lab');
  expect(view.changes).toEqual([
    {
      kind: 'Target',
      object: 'lab',
      action: 'update',
      detail: 'ssh key: ~/.ssh/lab.pub → ~/.ssh/work.pub',
    },
  ]);
  expect(result(await runPlan(view))).toEqual({ name: 'lab', token: null, sshKeyChanged: true });
  const after = await api.targetShow('lab');
  expect(after.sshKey?.display).toBe('~/.ssh/work.pub');
  expect(after.token).toEqual(before);
});

test('renew with nothing to change is refused, at the plan and when it runs', async () => {
  for (const sshKey of [null, '/home/alex/.ssh/id_ed25519.pub']) {
    const e = await refusalOf(api.opPlanTargetRenew('staging', null, sshKey));
    expect(e.code).toBe(CORE_ERROR_CODES.TARGET_RENEW_NOTHING_TO_CHANGE);
    expect(e.fields).toEqual({ name: 'staging' });
  }
  // Planned while the key differed, run once it no longer does.
  const late = await api.opPlanTargetRenew('lab', null, '/home/alex/.ssh/work.pub');
  await runPlan(await api.opPlanTargetRenew('lab', null, '/home/alex/.ssh/work.pub'));
  expect(failure(await runPlan(late))?.code).toBe(CORE_ERROR_CODES.TARGET_RENEW_NOTHING_TO_CHANGE);
});

test('doctor counts three stages for a target the store holds and two otherwise', async () => {
  const doctor = async (target: string) => {
    const opId = await api.opStartDoctor(target);
    const end = await runRead(opId);
    return { end: result(end) as { target: string; groups: { id: string }[] } };
  };
  const found = await doctor('lab');
  expect(found.end.target).toBe('lab');
  expect(found.end.groups.map((g) => g.id)).toEqual(['target', 'cluster', 'this_computer']);
  const missing = await doctor('ghost');
  expect(missing.end.groups.map((g) => g.id)).toEqual(['target', 'this_computer']);
  expect((await api.opList()).map((o) => o.title)).toContain('Doctor · ghost');
});

test('whoami: no ping in the plain read; the read pings and finds the token accepted', async () => {
  expect(await api.whoami()).toMatchObject({
    identity: 'anonymous_self_hosted',
    cliDefault: {
      status: 'found',
      target: { name: 'prod-eu', verification: { status: 'skipped', reason: 'no_ping' } },
    },
  });
  const pinged = result(await runRead(await api.opStartWhoami()));
  expect(pinged).toMatchObject({
    cliDefault: { target: { verification: { status: 'verified', elapsedMs: 182 } } },
  });
});
