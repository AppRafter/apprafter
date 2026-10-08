// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { ACTION_IDS } from './registry';

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
