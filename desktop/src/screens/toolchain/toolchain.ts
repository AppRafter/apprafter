// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The toolchain panel's words (spec §5.1: tool_not_found / cue_not_found lead here): the install
// lines for this OS (Linux shows every distribution's: the app cannot tell which one this is),
// what each probe found, and where the search path came from (overview decision 7). The facts
// are the core's probe (apprafter-core tools.rs) and doctor's tool row: a tool that is missing,
// or found only as a shim, or that cannot be started, is an error only when it is required; one
// that runs but prints no version is a warning, with its own words as the reason.
import type { HintOs } from '../../ipc/generated/HintOs';
import type { InstallHint } from '../../ipc/generated/InstallHint';
import type { Os } from '../../ipc/generated/Os';
import type { PathSource } from '../../ipc/generated/PathSource';
import type { ToolStatus } from '../../ipc/generated/ToolStatus';

export const HINT_LABELS: Record<HintOs, string> = {
  windows: 'Windows',
  macos: 'macOS',
  debian: 'Debian / Ubuntu',
  arch: 'Arch',
  nix: 'Nix',
  other: 'Other',
};

const FOR_OS: Record<Os, readonly HintOs[]> = {
  windows: ['windows'],
  macos: ['macos'],
  linux: ['debian', 'arch', 'nix', 'other'],
};

/** This OS's install lines; with none of its own, every line (each shows its label). */
export function hintsFor(hints: readonly InstallHint[], os: Os): InstallHint[] {
  const mine = hints.filter((h) => FOR_OS[os].includes(h.os));
  return mine.length > 0 ? mine : [...hints];
}

/**
 * The package managers cli-core's install lines start with. A line that starts with none of them
 * is a note to read ("preinstalled", the Windows optional feature), never a command to paste; a
 * new installer there fails toolchain.test.ts until it is named here.
 */
const INSTALLERS: readonly string[] = ['brew', 'apt', 'pacman', 'nix', 'winget', 'xcode-select'];

export type HintKind = 'command' | 'link' | 'note';

/** An install line is a command, a link to the tool's install page, or a note. */
export function hintKind(line: string): HintKind {
  if (/^https:\/\/\S+$/.test(line)) return 'link';
  return INSTALLERS.includes(line.split(' ', 1)[0] ?? '') ? 'command' : 'note';
}

/** A command or a link has something to copy; a note does not. */
export const copyable = (line: string): boolean => hintKind(line) !== 'note';

export interface StateLine {
  readonly tone: 'ok' | 'warn' | 'err';
  /** What the probe found, in the app's words (a found tool's: its version line). */
  readonly text: string;
  /** Why, in the tool's or the system's own words, when there are any: shown as they are. */
  readonly detail: string | null;
}

/** The core's TOOL_PROBE_TIMEOUT: the report says only that the probe timed out. */
const PROBE_TIMEOUT_S = 5;

export function toolStateLine(s: ToolStatus): StateLine {
  const p = s.problem;
  const missing = s.required ? 'err' : 'warn';
  if (p === null) return { tone: 'ok', text: s.version ?? 'Found, version unknown', detail: null };
  switch (p.kind) {
    case 'not_found':
      return { tone: missing, text: 'Not found on the search path', detail: null };
    case 'unsupported':
      return {
        tone: missing,
        text: `Found only as ${p.path}, which cannot be run directly: install the .exe build`,
        detail: null,
      };
    case 'no_version_output':
      return {
        tone: 'warn',
        text:
          p.exit === null
            ? 'Found, but it was stopped before it printed a version'
            : `Found, but it exited ${p.exit} without printing a version`,
        detail: p.detail,
      };
    case 'timed_out':
      return {
        tone: 'warn',
        text: `Found, but it did not print its version within ${PROBE_TIMEOUT_S} s`,
        detail: null,
      };
    case 'spawn_failed':
      return { tone: missing, text: 'It could not be started', detail: p.error };
  }
}

const SOURCES: Record<PathSource, string> = {
  environment: 'of the PATH this app was started with',
  login_shell: 'of the PATH your login shell sets',
  fallback: 'of a default PATH, because your login shell gave none',
  explicit: 'of a PATH set for this app',
};

export function pathSourceLine(source: PathSource, count: number): string {
  return `Searched ${count} ${count === 1 ? 'directory' : 'directories'} ${SOURCES[source]}`;
}
