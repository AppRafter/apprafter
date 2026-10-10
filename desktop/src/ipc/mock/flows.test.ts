// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The mock's richer answers for the wizard, the doctor and the toolchain (flows.ts), through the
// app's own calls, on D.3d's engine and store: the rules stay D.3d's.
import { afterEach, beforeEach, expect, test } from 'bun:test';
import { clearMocks } from '@tauri-apps/api/mocks';
import { settleIpc } from '../../test/settle';
import * as api from '../api';
import { CORE_ERROR_CODES } from '../generated/core-errors';
import type { DoctorReport } from '../generated/DoctorReport';
import { DESKTOP_ERROR_CODES } from '../generated/errors';
import type { MachineCatalogue } from '../generated/MachineCatalogue';
import type { RegionLatency } from '../generated/RegionLatency';
import type { TargetAdded } from '../generated/TargetAdded';
import type { TokenVerified } from '../generated/TokenVerified';
import { HETZNER_TOKEN_LEN } from '../generated/target';
import { operationsSnapshot, resetOperations, watchOperations } from '../operations';
import { failureOf, resultOf, runRead, startPlan } from '../plans';
import { MOCK_NOT_KEYS } from './fixtures';
import { MOCK_CATALOGUE, MOCK_LATENCIES, MOCK_REJECTED_TOKEN } from './flows';
import { installMockIpc, mockTargetStore } from './index';

beforeEach(async () => {
  installMockIpc({ opDelayMs: 0 });
  await api.unlock();
});
afterEach(async () => {
  await settleIpc();
  resetOperations();
  clearMocks();
});

const GOOD = 'A1'.repeat(HETZNER_TOKEN_LEN / 2);

const doctorOf = (target: string) => runRead<DoctorReport>(() => api.opStartDoctor(target));
const sshRow = (report: DoctorReport) =>
  report.groups.flatMap((group) => group.checks).find((check) => check.id === 'ssh_key');

test("verify follows D.3d's rules: a good token is a draft, an x-token the mock's 401", async () => {
  const ok = await runRead<TokenVerified>(() => api.opStartVerifyToken('hetzner-cloud', GOOD));
  expect(typeof ok.draftId).toBe('number');
  expect(MOCK_REJECTED_TOKEN.startsWith('x')).toBe(true);
  expect(MOCK_REJECTED_TOKEN).toHaveLength(HETZNER_TOKEN_LEN);
  const refused = failureOf(
    await runRead(() => api.opStartVerifyToken('hetzner-cloud', MOCK_REJECTED_TOKEN)).catch(
      (e: unknown) => e,
    ),
  );
  expect(refused.code).toBe(CORE_ERROR_CODES.TARGET_TOKEN_REJECTED);
});

test('the catalogue: an unknown draft or target is refused at the command; a verified draft reads the full catalogue', async () => {
  const refused = (source: Parameters<typeof api.opStartMachineCatalogue>[0]) =>
    api.opStartMachineCatalogue(source).then(
      () => null,
      (e: unknown) => failureOf(e).code,
    );
  expect(await refused({ kind: 'draft', draftId: 999 })).toBe(DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND);
  expect(await refused({ kind: 'target', name: 'ghost' })).toBe(CORE_ERROR_CODES.TARGET_NOT_FOUND);
  const { draftId } = await runRead<TokenVerified>(() =>
    api.opStartVerifyToken('hetzner-cloud', GOOD),
  );
  const cat = await runRead<MachineCatalogue>(() =>
    api.opStartMachineCatalogue({ kind: 'draft', draftId }),
  );
  expect(cat).toEqual(MOCK_CATALOGUE);
  expect(cat.regions.map((r) => r.code)).toEqual(['nbg1', 'fsn1', 'hel1', 'ash', 'hil', 'sin']);
  // The design's fifteen types in each European region, and one retired type besides.
  const nbg1 = cat.offers.filter((o) => o.location === 'nbg1');
  expect(nbg1).toHaveLength(16);
  expect(nbg1.filter((o) => !o.available).map((o) => o.sku)).toEqual(['cpx31', 'cx11']);
  expect(nbg1.filter((o) => o.retired).map((o) => o.sku)).toEqual(['cx11']);
  expect(
    cat.offers.filter((o) => o.deprecation !== null && !o.retired).map((o) => o.location),
  ).toContain('nbg1');
  expect(new Set(cat.offers.filter((o) => o.recommended).map((o) => o.sku))).toEqual(
    new Set(['cx32']),
  );
  // The Arm and the European-only shared x86 types are not offered in the US or Singapore.
  const ash = cat.offers.filter((o) => o.location === 'ash').map((o) => o.sku);
  expect(ash.some((sku) => sku.startsWith('cax') || sku.startsWith('cx'))).toBe(false);
  expect(ash.length).toBeGreaterThan(0);
  // The catalogue of a target reads the same.
  expect(
    await runRead<MachineCatalogue>(() =>
      api.opStartMachineCatalogue({ kind: 'target', name: 'staging' }),
    ),
  ).toEqual(MOCK_CATALOGUE);
});

test('latency: each region asked for, one of them unanswered', async () => {
  const regions = ['nbg1', 'hel1', 'sin'];
  const latencies = await runRead<RegionLatency[]>(() => api.opStartRegionLatencies(regions));
  expect(latencies).toEqual(MOCK_LATENCIES.filter((l) => regions.includes(l.region)));
  expect(latencies.find((l) => l.region === 'sin')?.latencyMs).toBeNull();
});

test("add: D.3d's handler takes the draft at planning; the run lists the new target", async () => {
  const { draftId } = await runRead<TokenVerified>(() =>
    api.opStartVerifyToken('hetzner-cloud', GOOD),
  );
  const args = {
    name: 'lab-2',
    provider: 'hetzner-cloud',
    draftId,
    sshKey: null,
    region: 'nbg1',
    tier: 'team',
    serverType: 'cx22',
  };
  const view = await api.opPlanTargetAdd(args);
  expect(view.class).toBe('bounded');
  const again = await api.opPlanTargetAdd(args).catch((e: unknown) => failureOf(e));
  expect((again as { code: string }).code).toBe(DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND);
  const added = resultOf(await (await startPlan(view.opId)).ended) as TargetAdded;
  expect(added.name).toBe('lab-2');
  expect(added.cliDefault).toBeNull();
  expect((await api.targetList()).targets.map((t) => t.name)).toContain('lab-2');
});

test('doctor on a provisioned target: three stages, then a report with every status', async () => {
  const seen: string[] = [];
  const off = watchOperations(() => {
    for (const view of operationsSnapshot().values()) {
      const title = view.stage?.title;
      if (title !== undefined && seen.at(-1) !== title) seen.push(title);
    }
  });
  const report = await doctorOf('prod-eu');
  off();
  expect(seen).toEqual(['Target', 'Cluster', 'This computer']);
  const statuses = new Set(report.groups.flatMap((g) => g.checks.map((c) => c.status)));
  expect([...statuses].sort()).toEqual(['fail', 'pass', 'skipped', 'warn']);
  expect(report.groups.map((g) => g.id)).toEqual(['target', 'cluster', 'this_computer']);
  // The missing helm of the toolchain, with the fix that opens it.
  const helm = report.groups[2]?.checks.find((c) => c.tool === 'helm');
  expect(helm).toMatchObject({ status: 'warn', fix: { kind: 'install_tool', tool: 'helm' } });
  expect(sshRow(report)).toMatchObject({ status: 'pass', fix: null });
  // No kubeconfig cached, so nothing to probe the API with: the two rows agree, as the core's do.
  expect(report.groups[1]?.checks.map((c) => [c.id, c.status])).toEqual([
    ['kubeconfig_cached', 'fail'],
    ['kube_api_reachable', 'skipped'],
    ['node_ssh_reachable', 'fail'],
  ]);
  expect(report.groups[1]?.checks[0]).toMatchObject({
    detail: 'none cached for server `prod-eu-1` (id 4711)',
    fix: { kind: 'fetch_kubeconfig', target: 'prod-eu' },
  });
});

test('doctor on a target with no server: the cluster checks are skipped', async () => {
  const report = await doctorOf('staging');
  expect(report.groups[1]?.checks.map((c) => c.status)).toEqual(['skipped', 'skipped', 'skipped']);
});

test("doctor's SSH key row says what the store holds: each of the three fixes", async () => {
  // lab's key file is gone (fixtures.ts).
  expect(sshRow(await doctorOf('lab'))).toMatchObject({
    status: 'fail',
    fix: { kind: 'ssh_key_missing', target: 'lab', path: '/home/alex/.ssh/lab.pub' },
  });
  // A target saved with no key (the wizard's Skip).
  const store = mockTargetStore();
  const staging = store.reports.get('staging');
  if (staging === undefined) throw new Error('no staging');
  store.reports.set('staging', { ...staging, sshKey: null });
  expect(sshRow(await doctorOf('staging'))).toMatchObject({
    status: 'warn',
    title: 'SSH key path configured',
    fix: { kind: 'configure_ssh_key', target: 'staging' },
  });
  // The key path pointed at the private half behind the app's back (an older CLI took it).
  const privateKey = MOCK_NOT_KEYS.find((key) => key.problem === 'private_key');
  store.reports.set('staging', { ...staging, sshKey: privateKey ?? null });
  expect(sshRow(await doctorOf('staging'))).toMatchObject({
    status: 'fail',
    fix: {
      kind: 'ssh_key_not_public',
      target: 'staging',
      path: privateKey?.path,
      privateKey: true,
    },
  });
});

test("doctor on a target the store does not hold is D.3d's: Add a target", async () => {
  const report = await doctorOf('nowhere');
  expect(report.groups[0]?.checks[0]?.fix).toMatchObject({ kind: 'add_target', name: 'nowhere' });
});

test('the toolchain: helm is missing, with a line for each system', async () => {
  const tools = await api.toolchainStatus();
  expect(tools.tools.map((t) => t.tool)).toEqual([
    'kubectl',
    'helm',
    'restic',
    'git',
    'ssh',
    'cue',
  ]);
  const helm = tools.tools.find((t) => t.tool === 'helm');
  expect(helm?.problem).toEqual({ kind: 'not_found' });
  expect(helm?.install.map((h) => h.os)).toEqual(
    expect.arrayContaining(['windows', 'macos', 'other']),
  );
  expect(tools.tools.filter((t) => t.problem !== null).map((t) => t.tool)).toEqual(['helm']);
});

test('the clipboard write lands where the Playwright walk reads it', async () => {
  const { invoke } = await import('@tauri-apps/api/core');
  await invoke('plugin:clipboard-manager|write_text', { text: 'report' });
  expect((window as unknown as { __mockClipboard?: string }).__mockClipboard).toBe('report');
});

test('no token-shaped string in flows.ts (overview §6.3)', async () => {
  const text = await Bun.file(new URL('./flows.ts', import.meta.url)).text();
  expect(text).not.toMatch(/[A-Za-z0-9]{64}/);
});
