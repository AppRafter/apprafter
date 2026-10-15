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
import type { CheckId } from '../../ipc/generated/CheckId';
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
  info: 'INFO',
};
export const STATUS_TONES = {
  pass: 'ok',
  warn: 'warn',
  fail: 'err',
  skipped: 'neutral',
  info: 'neutral',
} as const satisfies Record<CheckStatus, string>;

const RENEW = 'on the Target screen: API token › Renew.';
const RENEW_WHY: Record<RenewWhy, string> = {
  credentials_file_missing: `The credentials file is missing. Renew the token to write a new one, ${RENEW}`,
  token_missing: `No token is stored. Add one ${RENEW}`,
  token_malformed: `The stored token is malformed. Renew it with a fresh one ${RENEW}`,
  token_rejected: `The provider rejected the stored token. Create a new one in ${HETZNER_API_TOKENS_PAGE}, then renew it ${RENEW}`,
  token_outside_vault: `The credentials file holds this token in clear on a locked store, so it is not used. Renew it ${RENEW}`,
  token_in_another_vault: `The credentials file points at another target store's vault. Renew the token ${RENEW}`,
};

/**
 * The lock's machine-limit rows (D.4a T72, rule 1), in the app's words: the core's explanation
 * of each is written for a terminal (`apprafter lock now` in each one you unlocked); here the
 * person acts in the app or on the computer. Any other row's explanation is the core's.
 */
const LOCK_FIXES: Partial<Record<CheckId, string>> = {
  x11_keystrokes:
    'AppRafter runs as an X11 program here, so any program running as you can see what you type into it, the lock password included. Run AppRafter natively on Wayland — in a Wayland session, without `GDK_BACKEND=x11` — to close this.',
  ptrace_scope:
    'Set the kernel.yama.ptrace_scope sysctl to 1, so that only a parent process can attach to another. A kernel without the Yama module has no such setting: there, run AI agents under a separate OS account.',
  windows_same_user:
    "Run AI agents under a separate Standard Windows account: programs running as you can attach to an unlocked terminal, read what you type into AppRafter, and take over AppRafter's threads to read its memory while it holds this store's key.",
  macos_task_port:
    "Whether such a program can read another's memory depends on this Mac's debugging settings. Run AI agents under a separate Standard macOS account.",
  sleep_unreported:
    'Lock AppRafter yourself before the computer sleeps, and keep its auto-lock short.',
  secrets_in_use:
    'A kubeconfig AppRafter or a command is using is a file until it is done; one you saved stays where you saved it, outside the lock.',
  same_user_programs:
    'The lock stops an AI agent that oversteps; one that sets out to get past it — an agent turned by a prompt injection, for example — it only slows. Run AI agents under a separate OS account without administrator rights, or in a sandbox that cannot reach your terminals, files or ssh-agent.',
  hibernation:
    "Lock AppRafter, and end every terminal's unlock window, before this computer hibernates.",
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
      // No command (WI-457): the way back from a lost key is the CLI's, described as what it does.
      return `There is no age key at ${fix.path}, so the cached kubeconfig cannot be decrypted: restore the key it was cached with. If the key is lost, fetching the kubeconfig from the server again caches it under a new key, and what was cached under the lost one has to be fetched again too.`;
    case 'cache_undecryptable':
      // No command either (WI-457 review #8): what a lost key replaced by a new one leaves on
      // every other target, and the way back, described as what it does. On a locked store's own
      // key the server is the authentic source (D.4a T38): only the fetch.
      return fix.key === 'store_vault'
        ? "The cached kubeconfig does not open with this target store's own key: an older AppRafter or another program wrote it. Fetch the kubeconfig from the server again, which caches it under the store's key."
        : `The cached kubeconfig does not decrypt with the age key at ${fix.path}: it was cached under another key, or the copy is damaged. Restore the key it was cached with, or fetch the kubeconfig from the server again, which caches it under this key.`;
    case 'cluster_unreachable':
      return kubeText(fix.reason, target);
    case 'node_unreachable':
      return `Nothing accepted a connection on port 22 at ${fix.address}: check that the server is running and that no firewall between this computer and it blocks port 22.`;
    case 'dns':
      return `The resolver could not answer for \`${fix.host}\`: check the DNS settings, a VPN or a proxy.`;
    case 'explain':
      return LOCK_FIXES[check.id] ?? fix.text;
    // D.4a: the lock's fixes (surface §4.2); the rows that raise them are d4a-e's and f's.
    case 'protect_secrets':
      return 'Security › Protect secrets encrypts them.';
    case 'finish_lock':
      return fix.phase === 'unlocking'
        ? "Unprotecting this target store's secrets did not finish: finish it on the Security page."
        : "Protecting this target store's secrets did not finish: finish it on the Security page.";
    case 'unlock_first':
      return 'Unlock the app to run these checks.';
    case 'protect_ssh_key':
      return `${fix.path} reaches your servers and stays outside the lock: give it a passphrase, and keep it out of any ssh-agent other programs can reach.`;
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
