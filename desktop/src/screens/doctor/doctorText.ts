// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What the Doctor overlay says, from the core's neutral report. The CLI words its own hints from
// the same CheckFix (platform-cli render/doctor.rs); those words are not data, so the desktop
// writes its own from the fields, and takes its facts from the CLI and the core's checks (apprafter-
// core doctor.rs): a missing tool reads differently on its own row and on a row that needs it; a
// kubeconfig fails when none is cached and warns while an unencrypted copy remains; a skipped row
// is in no total (R8). A fix names a screen, never a CLI flag, but for the kubeconfig fetch, whose
// GUI action is D.12's.
import type { Check } from '../../ipc/generated/Check';
import type { CheckFix } from '../../ipc/generated/CheckFix';
import type { CheckStatus } from '../../ipc/generated/CheckStatus';
import type { DoctorReport } from '../../ipc/generated/DoctorReport';
import type { GroupId } from '../../ipc/generated/GroupId';
import type { KubeErrorKind } from '../../ipc/generated/KubeErrorKind';
import type { RenewWhy } from '../../ipc/generated/RenewWhy';
import { HETZNER_API_TOKENS_PAGE } from '../../ipc/generated/target';
import type { OpNote } from '../../ipc/plans';

export const GROUP_TITLES: Record<GroupId, string> = {
  target: 'Target',
  cluster: 'Cluster',
  this_computer: 'This computer',
};
export const STATUS_LABELS: Record<CheckStatus, string> = {
  pass: 'PASS',
  warn: 'WARN',
  fail: 'FAIL',
  skipped: 'SKIP',
};
export const STATUS_TONES = {
  pass: 'ok',
  warn: 'warn',
  fail: 'err',
  skipped: 'neutral',
} as const satisfies Record<CheckStatus, string>;

const RENEW = 'on the Target screen: API token › Renew.';
const RENEW_WHY: Record<RenewWhy, string> = {
  credentials_file_missing: `The credentials file is missing. Renew the token to write a new one, ${RENEW}`,
  token_missing: `No token is stored. Add one ${RENEW}`,
  token_malformed: `The stored token is malformed. Renew it with a fresh one ${RENEW}`,
  token_rejected: `The provider rejected the stored token. Create a new one in ${HETZNER_API_TOKENS_PAGE}, then renew it ${RENEW}`,
};

const TOOLCHAIN = 'The toolchain lists how for this computer.';

const kubeconfigCommand = (target: string | null, refresh: boolean) =>
  `\`apprafter kubeconfig${refresh ? ' --refresh' : ''} --target ${target ?? '<name>'}\``;

function kubeText(reason: KubeErrorKind, target: string | null): string {
  switch (reason) {
    case 'unreachable':
      return (
        "The cluster's API server did not answer. Check that the server is running and that " +
        'nothing between this computer and port 6443 blocks it: a refusal here usually means a ' +
        'stopped server or a stale kubeconfig.'
      );
    case 'forbidden':
      return `The cluster refused the cached kubeconfig: a server set up again leaves the old one behind. Fetch it again: ${kubeconfigCommand(target, true)}.`;
    case 'kind_not_served':
    case 'object_not_found':
    case 'other':
      return "kubectl could not read the cluster's version; its own message is in the detail above.";
  }
}

/**
 * The fix line of a row, or null for a row without a fix. `target` is the report's: the one
 * fix that has no target of its own (a refused kubeconfig) names it.
 */
export function fixText(check: Check, target: string | null): string | null {
  const fix: CheckFix | null = check.fix;
  if (fix === null) return null;
  switch (fix.kind) {
    case 'add_target':
      if (fix.name === null) return 'No target is set up yet: add one.';
      return `There is no target \`${fix.name}\`${fix.available.length > 0 ? ` (targets: ${fix.available.join(', ')})` : ''}: add it.`;
    case 'renew_token':
      return RENEW_WHY[fix.why];
    case 'chmod':
      return `Other accounts on this computer can read ${fix.path}: restrict it with \`chmod ${fix.mode.toString(8)} ${fix.path}\`.`;
    case 'unsupported_provider':
      return `The provider \`${fix.provider}\` is not supported by this build (supported: ${fix.supported.join(', ')}).`;
    case 'provider_error':
      return `The provider answered with an unexpected error (HTTP ${fix.status}). Try again; if it keeps failing, check the provider's status page.`;
    case 'provider_rate_limited':
      return 'The provider is rate-limiting requests (HTTP 429): wait a little, then run Doctor again.';
    case 'provider_unreachable':
      return "The provider's API could not be reached: check DNS, the network or a proxy.";
    case 'provider_request_failed':
      return (
        "The provider's answer could not be read, or the request could not be sent; the detail " +
        'says which. An answer that cannot be read comes from a proxy in between, or from a ' +
        "change in the provider's API that a newer AppRafter reads."
      );
    case 'configure_ssh_key':
      return 'No SSH key is set for this target: provisioning its server is refused until it has one.';
    case 'ssh_key_missing':
      return `There is no file at ${fix.path}: the stored path may be stale.`;
    case 'ssh_key_not_public':
      // GOTCHA-149: apply sends the key file as it is, so the core refuses one that is not a key.
      return fix.privateKey
        ? `${fix.path} is a private key, which is never sent to the provider: set its public half, the .pub file next to it.`
        : `${fix.path} is not an OpenSSH public key, so it is never sent to the provider: set a public key.`;
    case 'install_tool':
      // On the tool's own row a detail means it was found only as something it cannot run (a
      // `.cmd` shim); on another row the tool is what that check needs.
      if (check.id !== 'tool') {
        return `\`${fix.tool}\` is needed to reach the cluster: install it. ${TOOLCHAIN}`;
      }
      return check.detail === null
        ? `\`${fix.tool}\` is not on the search path: install it. ${TOOLCHAIN}`
        : `Install a build of \`${fix.tool}\` that runs directly. ${TOOLCHAIN}`;
    case 'fetch_kubeconfig':
      return check.status === 'fail'
        ? `Fetch it from the server, cached encrypted: ${kubeconfigCommand(fix.target, false)}.`
        : `Replace the unencrypted copy with an encrypted one: ${kubeconfigCommand(fix.target, true)}.`;
    case 'age_key_missing':
      return `There is no age key at ${fix.path}, so the cached kubeconfig cannot be decrypted: restore the key it was cached with.`;
    case 'cluster_unreachable':
      return kubeText(fix.reason, target);
    case 'node_unreachable':
      return `Nothing accepted a connection on port 22 at ${fix.address}: check that the server is running and that no firewall between this computer and it blocks port 22.`;
    case 'dns':
      return `The resolver could not answer for \`${fix.host}\`: check the DNS settings, a VPN or a proxy.`;
    case 'explain':
      return fix.text;
  }
}

export type InlinePart = { readonly text: string } | { readonly code: string };

/**
 * `a \`b\` c` → text, code, text (the core quotes names in backticks). A backtick with no
 * partner, as a tool's own words may hold, stays a backtick.
 */
export function inlineParts(text: string): InlinePart[] {
  const pieces = text.split('`');
  if (pieces.length % 2 === 0) {
    const open = pieces.pop() ?? '';
    pieces.push(`${pieces.pop() ?? ''}\`${open}`);
  }
  return pieces.flatMap((piece, index): InlinePart[] =>
    piece === '' ? [] : index % 2 === 1 ? [{ code: piece }] : [{ text: piece }],
  );
}

export interface CheckCounts {
  readonly pass: number;
  readonly warn: number;
  readonly fail: number;
  readonly skipped: number;
  /** The checks that ran: a skipped one counts in no total (R8). */
  readonly total: number;
}

export function checkCounts(report: DoctorReport): CheckCounts {
  const all: Check[] = report.groups.flatMap((g) => g.checks);
  const n = (s: CheckStatus) => all.filter((c) => c.status === s).length;
  const [pass, warn, fail] = [n('pass'), n('warn'), n('fail')];
  return { pass, warn, fail, skipped: n('skipped'), total: pass + warn + fail };
}

const pad = (n: number) => String(n).padStart(2, '0');

/** Local time, to the minute: `2026-10-09 14:02`. */
export const stamp = (at: Date) =>
  `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(at.getHours())}:${pad(at.getMinutes())}`;

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/**
 * The plain-text report Copy report writes: a header, what the core warned of while it ran (the
 * overlay's notes), each group's rows and fixes, the totals.
 */
export function reportText(report: DoctorReport, at: Date, notes: readonly OpNote[] = []): string {
  const lines = [`AppRafter doctor · ${report.target ?? 'no target'} · ${stamp(at)}`];
  if (notes.length > 0) {
    lines.push('', ...notes.map((n) => `  ${n.kind === 'warning' ? 'WARN' : 'NOTE'}  ${n.text}`));
  }
  for (const group of report.groups) {
    lines.push('', GROUP_TITLES[group.id]);
    for (const c of group.checks) {
      const detail = c.detail === null ? '' : ` — ${c.detail}`;
      // Every label is four letters, so the titles line up.
      lines.push(`  ${STATUS_LABELS[c.status]}  ${c.title}${detail}`);
      const fix = fixText(c, report.target);
      if (fix !== null) lines.push(`        → ${fix}`);
    }
  }
  const k = checkCounts(report);
  const skipped =
    k.skipped === 0 ? '' : ` ${plural(k.skipped, 'check was', 'checks were')} skipped.`;
  lines.push(
    '',
    `${plural(k.total, 'check', 'checks')}: ${k.pass} passed, ${plural(k.warn, 'warning', 'warnings')}, ${k.fail} failed.${skipped}`,
  );
  return `${lines.join('\n')}\n`;
}
