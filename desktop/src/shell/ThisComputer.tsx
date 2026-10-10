// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Settings › About › This computer (spec §7, whoami): the identity, the CLI's default target and
// whether its token authenticates. Opening Settings never pings (R13): the row reads whoami without
// one, and Verify runs the ping as an operation. Never verified unless the provider said so. A
// read that fails is shown, with its help; what an earlier read found stays, marked as such
// (review #6): never shown as just read.
import { useQuery } from '@tanstack/react-query';
import { Button } from '../components/Button';
import { SettingRow } from '../components/SettingRow';
import * as api from '../ipc/api';
import { uiErrorOf } from '../ipc/api';
import type { CliDefaultTarget } from '../ipc/generated/CliDefaultTarget';
import type { Identity } from '../ipc/generated/Identity';
import type { UiError } from '../ipc/generated/UiError';
import type { Verification } from '../ipc/generated/Verification';
import type { WhoamiReport } from '../ipc/generated/WhoamiReport';
import type { WhoamiTarget } from '../ipc/generated/WhoamiTarget';
import { keyValue } from '../screens/target/sshKey';
import { useRead } from '../state/read';

export const IDENTITY: Record<Identity, string> = {
  anonymous_self_hosted: 'anonymous (self-hosted)',
};

export interface VerificationLine {
  readonly tone: 'ok' | 'warn' | 'err' | 'neutral';
  readonly text: string;
}

/** What a token check found, in words; `ok` only for a token the provider accepted. */
export function verificationLine(v: Verification): VerificationLine {
  switch (v.status) {
    case 'verified':
      return { tone: 'ok', text: `Token verified · ${v.elapsedMs} ms` };
    case 'skipped':
      return v.reason === 'no_ping'
        ? { tone: 'neutral', text: 'Not checked yet' }
        : v.reason === 'no_token'
          ? { tone: 'warn', text: 'No token stored' }
          : { tone: 'neutral', text: 'No check exists for this provider' };
    case 'rejected':
      // The core's Rejected is the provider's 401 (provider.rs classify_ping_error).
      return { tone: 'err', text: 'Token rejected (HTTP 401)' };
    case 'rate_limited':
      return {
        tone: 'err',
        text: 'The provider is rate-limiting requests (HTTP 429): try again shortly',
      };
    case 'http_error':
      return { tone: 'err', text: `The provider answered HTTP ${v.httpStatus}` };
    case 'unreachable':
      // No answer at all: a provider that answered is one of the cases above or below.
      return { tone: 'err', text: 'Provider API unreachable' };
    case 'request_failed':
      return { tone: 'err', text: "The provider's answer could not be read: Doctor shows why" };
  }
}

export function cliDefaultLine(c: CliDefaultTarget): string {
  switch (c.status) {
    case 'none':
      return 'No CLI default: add a target, or make one the default on the Targets page.';
    case 'missing':
      return `The CLI default \`${c.name}\` does not exist${c.available.length > 0 ? ` (targets: ${c.available.join(', ')})` : ''}.`;
    case 'found':
      return `CLI default: ${c.target.name} · ${c.target.provider} · ${c.target.region ?? 'region not set'}`;
  }
}

/**
 * The default target's machine and key: `cx22 · ~/.ssh/id_ed25519.pub · ssh-ed25519`. The key
 * in D.3d's words (keyValue): a file that is gone, unreadable or not a public key says so.
 */
function machineLine(t: WhoamiTarget): string {
  const key = t.sshKey === null ? 'no SSH key' : keyValue(t.sshKey);
  return `${t.serverType ?? 'no server type'} · ${key}`;
}

/** A refusal in the row: what failed, and what to do about it when the core says. */
function ErrorLines({ error }: { error: UiError }) {
  return (
    <>
      <span data-tone="err">{error.message}</span>
      {error.help !== null && <span className="this-computer-help">{error.help}</span>}
    </>
  );
}

export function ThisComputerRow() {
  const query = useQuery({ queryKey: ['whoami'], queryFn: api.whoami });
  const verify = useRead<WhoamiReport>();
  // The ping's report, once it has one, replaces the read without a ping.
  const pinged = verify.state.status === 'done' ? verify.state.data : null;
  const report = pinged ?? query.data;
  const failure = query.isError ? uiErrorOf(query.error) : null;
  if (!report) {
    // `!report`: also the null a test's IPC mock answers for a command it does not know.
    return (
      <SettingRow
        label="This computer"
        sub={
          failure !== null ? (
            <span className="this-computer">
              <ErrorLines error={failure} />
            </span>
          ) : (
            'Reading…'
          )
        }
      />
    );
  }
  // The read again failed over a report read before: both, the older one said to be so.
  const earlier = pinged === null && failure !== null;
  const found = report.cliDefault.status === 'found' ? report.cliDefault.target : null;
  const line = found === null ? null : verificationLine(found.verification);
  const running = verify.state.status === 'running';
  return (
    <SettingRow
      label="This computer"
      sub={
        <span className="this-computer">
          {earlier && (
            <>
              <ErrorLines error={failure} />
              <span className="this-computer-earlier">From an earlier read:</span>
            </>
          )}
          <span>{IDENTITY[report.identity]}</span>
          <span>{cliDefaultLine(report.cliDefault)}</span>
          {found !== null && <span className="this-computer-machine">{machineLine(found)}</span>}
          {line !== null && <span data-tone={line.tone}>{line.text}</span>}
          {verify.state.status === 'failed' && <ErrorLines error={verify.state.error} />}
          {verify.state.status === 'cancelled' && (
            <span data-tone="neutral">The check was cancelled.</span>
          )}
        </span>
      }
      control={
        found === null ? undefined : (
          <Button
            size={28}
            pending={running}
            onClick={() => {
              void verify.run(api.opStartWhoami);
            }}
          >
            {running ? 'Verifying…' : 'Verify'}
          </Button>
        )
      }
    />
  );
}
