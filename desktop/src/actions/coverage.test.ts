// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { API_COMMANDS } from '../ipc/api';
import { ACTION_COMMANDS, ACTION_IDS, CLOSED_SLICES } from './registry';

type Node = { path: string[]; hidden: boolean };
type Entry = { action?: string; status?: string; slice?: string };

const commands: { commands: Node[] } = await Bun.file(
  new URL('../../../docs/reference/cli/commands.json', import.meta.url),
).json();
const coverage = Bun.TOML.parse(
  await Bun.file(new URL('../../coverage.toml', import.meta.url)).text(),
) as { leaf?: Record<string, Entry> };

const STATUSES: readonly string[] = ['planned', 'cli-only', 'unavailable', 'superseded'];

/** Visible leaves: not hidden, no hidden ancestor, nothing one segment longer below. */
function visibleLeaves(nodes: Node[]): string[] {
  const key = (p: string[]) => p.join(' ');
  const hidden = new Set(nodes.filter((n) => n.hidden).map((n) => key(n.path)));
  const hasHiddenAncestor = (p: string[]) => p.some((_, i) => hidden.has(key(p.slice(0, i + 1))));
  const isParent = (p: string[]) =>
    nodes.some((n) => n.path.length === p.length + 1 && key(n.path.slice(0, p.length)) === key(p));
  return nodes
    .filter((n) => n.path.length > 0 && !hasHiddenAncestor(n.path) && !isParent(n.path))
    .map((n) => key(n.path))
    .sort();
}

const leaves = visibleLeaves(commands.commands);
const entries = coverage.leaf ?? {};

test('every visible CLI leaf is mapped, and nothing else is', () => {
  expect(Object.keys(entries).sort()).toEqual(leaves);
});

test('each entry is either an action or a status, never both', () => {
  for (const [leaf, e] of Object.entries(entries)) {
    const hasAction = typeof e.action === 'string';
    const hasStatus = typeof e.status === 'string';
    expect({ leaf, exactlyOne: hasAction !== hasStatus }).toEqual({ leaf, exactlyOne: true });
    if (hasStatus) {
      expect({ leaf, known: STATUSES.includes(e.status ?? '') }).toEqual({ leaf, known: true });
    }
    if (e.status === 'planned') {
      expect({ leaf, slice: /^D\.\d+$/.test(e.slice ?? '') }).toEqual({ leaf, slice: true });
    }
  }
});

test('every action id exists in the registry', () => {
  const known = new Set<string>(ACTION_IDS);
  for (const [leaf, e] of Object.entries(entries)) {
    if (e.action !== undefined) {
      expect({ leaf, known: known.has(e.action) }).toEqual({ leaf, known: true });
    }
  }
});

test('every action id runs a command the shell registers', () => {
  expect(Object.keys(ACTION_COMMANDS).sort()).toEqual([...ACTION_IDS].sort());
  const commands = new Set<string>(API_COMMANDS);
  for (const id of ACTION_IDS) {
    expect({ id, registered: commands.has(ACTION_COMMANDS[id]) }).toEqual({ id, registered: true });
  }
});

test('a closed slice has no planned leaf left', () => {
  for (const [leaf, e] of Object.entries(entries)) {
    if (e.slice !== undefined && CLOSED_SLICES.includes(e.slice)) {
      expect({ leaf, status: e.status ?? null }).not.toEqual({ leaf, status: 'planned' });
    }
  }
});

/** D.3's nine leaves, each with the action that covers it (docs/status.md names them). */
const D3_LEAVES = [
  ['target add', 'target.add'],
  ['target list', 'target.list'],
  ['target show', 'target.show'],
  ['target use', 'target.use'],
  ['target rename', 'target.rename'],
  ['target remove', 'target.remove'],
  ['target machine', 'target.machine'],
  ['doctor', 'doctor.run'],
  ['whoami', 'whoami.show'],
] as const;

// The closed-slice check above sees only the leaves of a slice it is told is closed: D.3 must
// stay closed, and its nine leaves stay actions in it — re-planned under a later slice, or
// back to planned, a leaf the app shows would no longer be held to its action.
test('D.3 stays closed, and its nine leaves stay actions in it', () => {
  expect(CLOSED_SLICES).toContain('D.3');
  for (const [leaf, id] of D3_LEAVES) {
    expect({
      leaf,
      action: entries[leaf]?.action,
      slice: entries[leaf]?.slice,
      status: entries[leaf]?.status ?? null,
    }).toEqual({ leaf, action: id, slice: 'D.3', status: null });
  }
  const inD3 = Object.entries(entries)
    .filter(([, e]) => e.slice === 'D.3')
    .map(([leaf]) => leaf)
    .sort();
  expect(inD3).toEqual(D3_LEAVES.map(([leaf]) => leaf).sort());
});

test('the fixed markers stay what the spec decided', () => {
  expect(entries.completion?.status).toBe('cli-only');
  for (const leaf of ['plan', 'login', 'upgrade-tier']) {
    expect(entries[leaf]?.status).toBe('unavailable');
  }
  expect(entries.init?.status).toBe('superseded');
});

test('report how much is still planned', () => {
  const planned = Object.values(entries).filter((e) => e.status === 'planned').length;
  console.log(`coverage: ${leaves.length} visible leaves, ${planned} planned`);
  expect(leaves.length).toBeGreaterThan(0);
});
