// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import type { CheckFix } from '../../ipc/generated/CheckFix';
import { check, doctorReport } from '../../test/flows';
import {
  checkCounts,
  fixText,
  GROUP_TITLES,
  inlineParts,
  reportText,
  STATUS_LABELS,
  STATUS_TONES,
  stamp,
} from './doctorText';

const fixed = (fix: CheckFix, more: Parameters<typeof check>[0] = {}) =>
  fixText(check({ status: 'fail', fix, ...more }), 'prod-eu');

describe('counts and labels', () => {
  test('per status; a skipped row is in no total (R8)', () => {
    expect(checkCounts(doctorReport())).toEqual({
      pass: 3,
      warn: 2,
      fail: 1,
      skipped: 1,
      total: 6,
    });
  });

  test('group titles, status labels and tones', () => {
    expect(GROUP_TITLES).toEqual({
      target: 'Target',
      cluster: 'Cluster',
      this_computer: 'This computer',
    });
    expect(STATUS_LABELS).toEqual({ pass: 'PASS', warn: 'WARN', fail: 'FAIL', skipped: 'SKIP' });
    expect(STATUS_TONES).toEqual({ pass: 'ok', warn: 'warn', fail: 'err', skipped: 'neutral' });
  });

  test('a local time stamp, minutes, zero-padded', () => {
    expect(stamp(new Date(2026, 0, 5, 9, 7))).toBe('2026-01-05 09:07');
  });
});

describe('inlineParts', () => {
  test('backticks become code spans', () => {
    expect(inlineParts('`kubectl` on PATH')).toEqual([{ code: 'kubectl' }, { text: ' on PATH' }]);
    expect(inlineParts('DNS resolves `api.hetzner.cloud`')).toEqual([
      { text: 'DNS resolves ' },
      { code: 'api.hetzner.cloud' },
    ]);
    expect(inlineParts('no code')).toEqual([{ text: 'no code' }]);
  });

  test("a backtick with no partner stays a backtick (a tool's own words)", () => {
    expect(inlineParts("command not found: `foo'")).toEqual([{ text: "command not found: `foo'" }]);
    expect(inlineParts('a `b` c `d')).toEqual([{ text: 'a ' }, { code: 'b' }, { text: ' c `d' }]);
  });
});

describe('fixText', () => {
  test('a row without a fix has no fix text', () => {
    expect(fixText(check(), 'prod-eu')).toBeNull();
  });

  test('a token to renew names the Target screen, and for a rejected one where to get another', () => {
    expect(fixed({ kind: 'renew_token', target: 'prod-eu', why: 'token_rejected' })).toBe(
      'The provider rejected the stored token. Create a new one in ' +
        'the Hetzner Console (open the project, then Security → API tokens), then renew it ' +
        'on the Target screen: API token › Renew.',
    );
    for (const why of ['credentials_file_missing', 'token_missing', 'token_malformed'] as const) {
      expect(fixed({ kind: 'renew_token', target: 'prod-eu', why })).toEndWith(
        'on the Target screen: API token › Renew.',
      );
    }
  });

  test('a missing tool: its own row says it is missing, or found in a form that cannot run; another row says what needs it', () => {
    const tool = { id: 'tool', tool: 'helm' } as const;
    expect(fixed({ kind: 'install_tool', tool: 'helm' }, tool)).toBe(
      '`helm` is not on the search path: install it. The toolchain lists how for this computer.',
    );
    expect(
      fixed(
        { kind: 'install_tool', tool: 'cue' },
        { id: 'tool', tool: 'cue', detail: 'C:\\tools\\cue.cmd cannot be run directly' },
      ),
    ).toBe(
      'Install a build of `cue` that runs directly. The toolchain lists how for this computer.',
    );
    expect(
      fixed(
        { kind: 'install_tool', tool: 'kubectl' },
        { id: 'kube_api_reachable', status: 'skipped', detail: '`kubectl` not found' },
      ),
    ).toBe(
      '`kubectl` is needed to reach the cluster: install it. The toolchain lists how for this computer.',
    );
  });

  test('a kubeconfig to fetch: none cached is fetched; an unencrypted copy is replaced', () => {
    expect(fixed({ kind: 'fetch_kubeconfig', target: 'prod-eu' })).toBe(
      'Fetch it from the server, cached encrypted: `apprafter kubeconfig --target prod-eu`.',
    );
    expect(fixed({ kind: 'fetch_kubeconfig', target: 'prod-eu' }, { status: 'warn' })).toBe(
      'Replace the unencrypted copy with an encrypted one: ' +
        '`apprafter kubeconfig --refresh --target prod-eu`.',
    );
  });

  test("a cluster that did not answer, refused the kubeconfig, or failed otherwise (kubectl's classes)", () => {
    const kube = (reason: 'unreachable' | 'forbidden' | 'kind_not_served' | 'other') =>
      fixed({ kind: 'cluster_unreachable', reason });
    expect(kube('unreachable')).toBe(
      "The cluster's API server did not answer. Check that the server is running and that " +
        'nothing between this computer and port 6443 blocks it: a refusal here usually means a ' +
        'stopped server or a stale kubeconfig.',
    );
    expect(kube('forbidden')).toBe(
      'The cluster refused the cached kubeconfig: a server set up again leaves the old one ' +
        'behind. Fetch it again: `apprafter kubeconfig --refresh --target prod-eu`.',
    );
    expect(
      fixText(check({ fix: { kind: 'cluster_unreachable', reason: 'forbidden' } }), null),
    ).toContain('`apprafter kubeconfig --refresh --target <name>`');
    for (const reason of ['kind_not_served', 'other'] as const) {
      expect(kube(reason)).toBe(
        "kubectl could not read the cluster's version; its own message is in the detail above.",
      );
    }
  });

  test('the other fixes, from their fields', () => {
    expect(fixed({ kind: 'add_target', name: null, available: [] })).toBe(
      'No target is set up yet: add one.',
    );
    expect(fixed({ kind: 'add_target', name: 'prod', available: ['lab', 'staging'] })).toBe(
      'There is no target `prod` (targets: lab, staging): add it.',
    );
    expect(fixed({ kind: 'add_target', name: 'prod', available: [] })).toBe(
      'There is no target `prod`: add it.',
    );
    expect(fixed({ kind: 'chmod', path: '/c/credentials.yaml', mode: 0o600 })).toBe(
      'Other accounts on this computer can read /c/credentials.yaml: restrict it with ' +
        '`chmod 600 /c/credentials.yaml`.',
    );
    expect(fixed({ kind: 'provider_error', status: 503 })).toBe(
      'The provider answered with an unexpected error (HTTP 503). Try again; if it keeps ' +
        "failing, check the provider's status page.",
    );
    // A provider that answered is never "unreachable" (WI-453 follow-up).
    expect(fixed({ kind: 'provider_rate_limited' })).toBe(
      'The provider is rate-limiting requests (HTTP 429): wait a little, then run Doctor again.',
    );
    expect(fixed({ kind: 'provider_request_failed' })).toBe(
      "The provider's answer could not be read, or the request could not be sent; the detail " +
        'says which. An answer that cannot be read comes from a proxy in between, or from a ' +
        "change in the provider's API that a newer AppRafter reads.",
    );
    expect(fixed({ kind: 'node_unreachable', address: '203.0.113.10' })).toBe(
      'Nothing accepted a connection on port 22 at 203.0.113.10: check that the server is ' +
        'running and that no firewall between this computer and it blocks port 22.',
    );
    expect(fixed({ kind: 'age_key_missing', path: '/k/age.key' })).toBe(
      'There is no age key at /k/age.key, so the cached kubeconfig cannot be decrypted: ' +
        'restore the key it was cached with.',
    );
    expect(fixed({ kind: 'explain', text: '`helm` exited 1 without printing a version' })).toBe(
      '`helm` exited 1 without printing a version',
    );
  });

  test('a key file that is not a public key: a private key names its public half', () => {
    expect(
      fixed({
        kind: 'ssh_key_not_public',
        target: 'prod-eu',
        path: '/home/alex/.ssh/id_ed25519',
        privateKey: true,
      }),
    ).toBe(
      '/home/alex/.ssh/id_ed25519 is a private key, which is never sent to the provider: set its public half, the .pub file next to it.',
    );
    expect(
      fixed({
        kind: 'ssh_key_not_public',
        target: 'prod-eu',
        path: '/notes.txt',
        privateKey: false,
      }),
    ).toBe(
      '/notes.txt is not an OpenSSH public key, so it is never sent to the provider: set a public key.',
    );
  });

  test('a fix names a screen or a fact, never a CLI flag, but for the kubeconfig fetch (D.12)', () => {
    const fixes: CheckFix[] = [
      { kind: 'add_target', name: 'prod', available: ['lab'] },
      { kind: 'renew_token', target: 'prod-eu', why: 'token_missing' },
      { kind: 'chmod', path: '/c', mode: 0o600 },
      { kind: 'unsupported_provider', provider: 'aws', supported: ['hetzner-cloud'] },
      { kind: 'provider_error', status: 500 },
      { kind: 'provider_rate_limited' },
      { kind: 'provider_unreachable' },
      { kind: 'provider_request_failed' },
      { kind: 'configure_ssh_key', target: 'prod-eu' },
      { kind: 'ssh_key_missing', target: 'prod-eu', path: '/k.pub' },
      { kind: 'ssh_key_not_public', target: 'prod-eu', path: '/k', privateKey: true },
      { kind: 'install_tool', tool: 'helm' },
      { kind: 'age_key_missing', path: '/k' },
      { kind: 'cluster_unreachable', reason: 'unreachable' },
      { kind: 'node_unreachable', address: '203.0.113.10' },
      { kind: 'dns', host: 'api.hetzner.cloud' },
    ];
    for (const fix of fixes) {
      const text = fixed(fix);
      expect(text, fix.kind).not.toBeNull();
      expect(text, fix.kind).not.toContain('--');
      expect(text, fix.kind).not.toContain('apprafter ');
    }
  });
});

describe('reportText', () => {
  test('header, groups, rows with detail and fix, a skipped row with its reason, totals', () => {
    const text = reportText(doctorReport(), new Date(2026, 9, 9, 14, 2));
    expect(text.split('\n').slice(0, 4)).toEqual([
      'AppRafter doctor · prod-eu · 2026-10-09 14:02',
      '',
      'Target',
      '  PASS  Config file readable — ~/.config/apprafter/targets/prod-eu/config.yaml',
    ]);
    expect(text).toContain(
      '  FAIL  Token verified against provider API — HTTP 401: Unable to authenticate\n' +
        '        → The provider rejected the stored token.',
    );
    expect(text).toContain('  WARN  SSH key path configured\n        → No SSH key');
    expect(text).toContain('\nCluster\n  SKIP  Kubeconfig cached — no provisioned server\n');
    expect(text).toContain(
      '\nThis computer\n  PASS  `kubectl` on PATH — Client Version: v1.34.1\n',
    );
    expect(text.endsWith('\n')).toBe(true);
    expect(text.trimEnd().split('\n').at(-1)).toBe(
      '6 checks: 3 passed, 2 warnings, 1 failed. 1 check was skipped.',
    );
  });

  test("the core's warnings and notices of the run follow the header, before the groups", () => {
    const text = reportText(doctorReport(), new Date(2026, 9, 9, 14, 2), [
      { kind: 'warning', text: 'cannot remove old kubeconfig copies' },
      { kind: 'notice', text: 'kubectl answered slowly' },
    ]);
    expect(text.split('\n').slice(0, 6)).toEqual([
      'AppRafter doctor · prod-eu · 2026-10-09 14:02',
      '',
      '  WARN  cannot remove old kubeconfig copies',
      '  NOTE  kubectl answered slowly',
      '',
      'Target',
    ]);
  });

  test('no target, one check, nothing skipped', () => {
    const text = reportText(
      { target: null, groups: [{ id: 'this_computer', checks: [check({ id: 'dns' })] }] },
      new Date(2026, 9, 9, 14, 2),
    );
    expect(text.startsWith('AppRafter doctor · no target · ')).toBe(true);
    expect(text.trimEnd().split('\n').at(-1)).toBe('1 check: 1 passed, 0 warnings, 0 failed.');
  });
});
