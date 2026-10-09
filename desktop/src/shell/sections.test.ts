// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The slice each placeholder names is the slice coverage.toml plans for the CLI command behind
// the section, so the two cannot drift.
import { expect, test } from 'bun:test';
import type { Section } from '../state/session';
import { SECTIONS } from './sections';

const coverage = Bun.TOML.parse(
  await Bun.file(new URL('../../coverage.toml', import.meta.url)).text(),
) as { leaf: Record<string, { status?: string; slice?: string }> };

const ALL: Section[] = [
  'overview',
  'apps',
  'approvals',
  'backups',
  'data',
  'network',
  'platform',
  'nodes',
  'target',
];

test('every section is listed once, main ones first', () => {
  expect(SECTIONS.map((s) => s.id)).toEqual(ALL);
  expect(SECTIONS.filter((s) => s.group === 'main').map((s) => s.id)).toEqual([
    'overview',
    'apps',
    'approvals',
  ]);
});

test("each section's slice is coverage.toml's for its command, and its copy names it", () => {
  for (const section of SECTIONS) {
    const entry = coverage.leaf[section.leaf];
    expect(entry, section.leaf).toBeDefined();
    expect(entry?.status, section.leaf).toBe('planned');
    expect(entry?.slice, section.id).toBe(section.slice);
    expect(section.planned, section.id).toContain(`in ${section.slice}.`);
  }
});

const reference = (await Bun.file(
  new URL('../../../docs/reference/cli/commands.json', import.meta.url),
).json()) as { commands: { path: string[]; positionals: { id: string }[] }[] };

test('a section names the tab’s target in its CLI hint exactly when the command takes a name', () => {
  for (const section of SECTIONS) {
    const command = reference.commands.find((c) => c.path.join(' ') === section.leaf);
    expect(command, section.leaf).toBeDefined();
    const takesName = command?.positionals.some((p) => p.id === 'name') ?? false;
    expect(section.named ?? false, section.leaf).toBe(takesName);
  }
});
