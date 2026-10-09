// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { join } from 'node:path';
import type { ToolId } from '../../ipc/generated/ToolId';
import type { ToolStatus } from '../../ipc/generated/ToolStatus';
import { toolchainReport } from '../../test/flows';
import {
  copyable,
  HINT_LABELS,
  hintKind,
  hintsFor,
  pathSourceLine,
  toolStateLine,
} from './toolchain';

const tool = (id: ToolId): ToolStatus => {
  const found = toolchainReport().tools.find((t) => t.tool === id);
  if (found === undefined) throw new Error(`no ${id} in the fixture`);
  return found;
};

describe('install lines', () => {
  test('per OS: Windows and macOS their own; Linux every distribution line and the link', () => {
    expect(hintsFor(tool('helm').install, 'windows').map((h) => h.command)).toEqual([
      'winget install Helm.Helm',
    ]);
    expect(hintsFor(tool('helm').install, 'macos').map((h) => h.command)).toEqual([
      'brew install helm',
    ]);
    expect(hintsFor(tool('helm').install, 'linux').map((h) => h.os)).toEqual([
      'debian',
      'nix',
      'other',
    ]);
    expect(hintsFor(tool('cue').install, 'linux').map((h) => h.os)).toEqual([
      'arch',
      'nix',
      'other',
    ]);
  });

  test('an OS with no line of its own gets every line, each with its label', () => {
    expect(hintsFor([{ os: 'other', command: 'https://x' }], 'windows').map((h) => h.os)).toEqual([
      'other',
    ]);
    expect(
      hintsFor(
        [
          { os: 'debian', command: 'apt install x' },
          { os: 'other', command: 'https://x' },
        ],
        'macos',
      ).map((h) => h.os),
    ).toEqual(['debian', 'other']);
    expect(HINT_LABELS).toEqual({
      windows: 'Windows',
      macos: 'macOS',
      debian: 'Debian / Ubuntu',
      arch: 'Arch',
      nix: 'Nix',
      other: 'Other',
    });
  });

  test('a command and a link can be copied; "preinstalled" and a sentence cannot', () => {
    expect(hintKind('brew install helm')).toBe('command');
    expect(hintKind('xcode-select --install')).toBe('command');
    expect(hintKind('https://helm.sh/docs/intro/install/')).toBe('link');
    expect(hintKind('preinstalled')).toBe('note');
    expect(
      hintKind('built into Windows 10/11: Settings › Optional features › OpenSSH Client'),
    ).toBe('note');
    expect(copyable('brew install helm')).toBe(true);
    expect(copyable('https://helm.sh/docs/intro/install/')).toBe(true);
    expect(copyable('preinstalled')).toBe(false);
  });

  test("a timed-out probe's seconds are the core's TOOL_PROBE_TIMEOUT", async () => {
    // The report says only that the probe timed out; the bound is apprafter-core's constant.
    const rust = await Bun.file(
      join(import.meta.dir, '../../../../cli/apprafter-core/src/tools.rs'),
    ).text();
    const secs = rust.match(
      /pub const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs\((\d+)\);/,
    )?.[1];
    expect(secs, 'TOOL_PROBE_TIMEOUT in apprafter-core/src/tools.rs').toBeDefined();
    const line = toolStateLine({
      tool: 'ssh',
      required: false,
      purpose: 'reaching the node over SSH',
      path: '/usr/bin/ssh',
      version: null,
      problem: { kind: 'timed_out' },
      install: [],
    });
    expect(line.text).toBe(`Found, but it did not print its version within ${secs} s`);
  });

  test('every install line the CLI ships is a command, a link or one of its two notes', async () => {
    // cli-core's tool definitions are the source of every line the core sends. A new installer
    // there fails here, rather than losing its Copy button quietly.
    const rust = await Bun.file(
      join(import.meta.dir, '../../../../cli/cli-core/src/tools.rs'),
    ).text();
    const lines = [...rust.matchAll(/^\s*text: "((?:[^"\\]|\\.)*)",$/gm)].map(([, t = '']) => t);
    expect(lines.length).toBeGreaterThan(25);
    expect(lines.filter((l) => hintKind(l) === 'note').sort()).toEqual([
      'built into Windows 10/11: Settings › Optional features › OpenSSH Client',
      'preinstalled',
    ]);
  });
});

describe('state lines', () => {
  test('found: the version line; found with no version line says so', () => {
    expect(toolStateLine(tool('kubectl'))).toEqual({
      tone: 'ok',
      text: 'Client Version: v1.34.1',
      detail: null,
    });
    expect(toolStateLine({ ...tool('kubectl'), version: null })).toEqual({
      tone: 'ok',
      text: 'Found, version unknown',
      detail: null,
    });
  });

  test('missing, or found only as a shim: an error when required, else a warning', () => {
    expect(toolStateLine(tool('helm'))).toEqual({
      tone: 'warn',
      text: 'Not found on the search path',
      detail: null,
    });
    expect(toolStateLine({ ...tool('helm'), required: true }).tone).toBe('err');
    expect(toolStateLine(tool('cue'))).toEqual({
      tone: 'warn',
      text: 'Found only as C:\\tools\\cue.cmd, which cannot be run directly: install the .exe build',
      detail: null,
    });
    expect(toolStateLine({ ...tool('cue'), required: true }).tone).toBe('err');
  });

  test('it ran and gave no version: a warning with its exit and its own words', () => {
    expect(toolStateLine(tool('git'))).toEqual({
      tone: 'warn',
      text: 'Found, but it exited 1 without printing a version',
      detail: 'xcrun: error: invalid active developer path (/Library/Developer/CommandLineTools)',
    });
    expect(
      toolStateLine({
        ...tool('git'),
        required: true,
        problem: { kind: 'no_version_output', exit: null, detail: null },
      }),
    ).toEqual({
      tone: 'warn',
      text: 'Found, but it was stopped before it printed a version',
      detail: null,
    });
  });

  test('timed out, or could not be started (with why)', () => {
    expect(toolStateLine(tool('ssh'))).toEqual({
      tone: 'warn',
      text: 'Found, but it did not print its version within 5 s',
      detail: null,
    });
    const spawn = { kind: 'spawn_failed', error: 'Permission denied (os error 13)' } as const;
    expect(toolStateLine({ ...tool('ssh'), problem: spawn })).toEqual({
      tone: 'warn',
      text: 'It could not be started',
      detail: 'Permission denied (os error 13)',
    });
    expect(toolStateLine({ ...tool('ssh'), required: true, problem: spawn }).tone).toBe('err');
  });
});

describe('the search path', () => {
  test('how many directories, and where the path came from', () => {
    expect(pathSourceLine('login_shell', 2)).toBe(
      'Searched 2 directories of the PATH your login shell sets',
    );
    expect(pathSourceLine('fallback', 4)).toBe(
      'Searched 4 directories of a default PATH, because your login shell gave none',
    );
    expect(pathSourceLine('environment', 1)).toBe(
      'Searched 1 directory of the PATH this app was started with',
    );
    expect(pathSourceLine('explicit', 3)).toBe('Searched 3 directories of a PATH set for this app');
  });
});
