// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The slice each placeholder names is the slice coverage.toml plans for the CLI command behind
// the section, so the two cannot drift.
import { expect, test } from 'bun:test';
import type { Section } from '../state/session';
import { SECTIONS } from './sections';

const coverage = Bun.TOML.parse(
  await Bun.file(new URL('../../coverage.toml', import.meta.url)).text(),
) as { leaf: Record<string, { action?: string; status?: string; slice?: string }> };

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

test("each section's slice is coverage.toml's for its command; a screen's leaf is an action", () => {
  for (const section of SECTIONS) {
    const entry = coverage.leaf[section.leaf];
    expect(entry, section.leaf).toBeDefined();
    expect(entry?.slice, section.id).toBe(section.slice);
    if (section.screen === true) {
      expect(entry?.action, section.leaf).toBeDefined();
      expect(section.planned, section.id).toBeUndefined();
    } else {
      expect(entry?.status, section.leaf).toBe('planned');
      expect(section.planned, section.id).toContain(`in ${section.slice}.`);
    }
  }
  expect(SECTIONS.filter((s) => s.screen === true).map((s) => s.id)).toEqual(['target']);
});

const reference = (await Bun.file(
  new URL('../../../docs/reference/cli/commands.json', import.meta.url),
).json()) as { commands: { path: string[]; positionals: { id: string }[] }[] };

test('every planned section runs on the CLI’s active target, so its hint says to switch first', () => {
  for (const section of SECTIONS.filter((s) => s.screen !== true)) {
    const command = reference.commands.find((c) => c.path.join(' ') === section.leaf);
    expect(command, section.leaf).toBeDefined();
    expect(command?.positionals.some((p) => p.id === 'name') ?? false, section.leaf).toBe(false);
  }
});
