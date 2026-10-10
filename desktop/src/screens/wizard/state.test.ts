// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { catalogue, targetAdded } from '../../test/flows';
import { addArgs, addedMessage, initialWizard, type WizardState, wizardReducer } from './state';

const TOKEN = 'A1'.repeat(32);
const run = (actions: Parameters<typeof wizardReducer>[1][], from: WizardState = initialWizard()) =>
  actions.reduce(wizardReducer, from);

describe('wizardReducer', () => {
  test('a verified token leaves only the draft: the field is emptied', () => {
    const s = run([
      { type: 'token', value: TOKEN },
      { type: 'verified', draft: 7 },
    ]);
    expect(s.token).toBe('');
    expect(s.draft).toBe(7);
    expect(JSON.stringify(s)).not.toContain(TOKEN);
  });

  test('a lost draft goes back to the provider step, the choices kept', () => {
    const s = run([
      { type: 'verified', draft: 7 },
      { type: 'catalogue', catalogue: catalogue() },
      { type: 'name', value: 'lab' },
      { type: 'go', step: 2 },
      { type: 'forgetDraft' },
    ]);
    expect([s.step, s.draft, s.catalogue, s.name, s.region]).toEqual([
      0,
      null,
      null,
      'lab',
      'nbg1',
    ]);
  });

  test('the plan took the draft: it is gone, the step stays', () => {
    const s = run([
      { type: 'verified', draft: 7 },
      { type: 'go', step: 2 },
      { type: 'draftTaken' },
    ]);
    expect([s.step, s.draft]).toEqual([2, null]);
  });

  test("the catalogue's arrival picks nbg1 and its recommended offer", () => {
    const s = run([
      { type: 'verified', draft: 7 },
      { type: 'catalogue', catalogue: catalogue() },
    ]);
    expect([s.region, s.sku]).toEqual(['nbg1', 'cx22']);
  });

  test('another region keeps an offer it has, else takes its recommended one or none', () => {
    const base = run([
      { type: 'verified', draft: 7 },
      { type: 'catalogue', catalogue: catalogue() },
    ]);
    expect(run([{ type: 'region', value: 'hel1' }], base).sku).toBe('cx22');
    expect(run([{ type: 'region', value: 'fsn1' }], base).sku).toBeNull();
  });

  test('another provider drops the draft', () => {
    expect(
      run([
        { type: 'verified', draft: 7 },
        { type: 'provider', value: 'other' },
      ]).draft,
    ).toBeNull();
  });
});

describe('addArgs and the toast', () => {
  test('no token field, ever; null until a draft, region, machine and tier are there', () => {
    expect(addArgs(initialWizard(), null)).toBeNull();
    const s = run([
      { type: 'verified', draft: 7 },
      { type: 'catalogue', catalogue: catalogue() },
      { type: 'name', value: 'lab' },
    ]);
    const args = addArgs(s, '/home/alex/.ssh/id_ed25519.pub');
    expect(args).toEqual({
      name: 'lab',
      provider: 'hetzner-cloud',
      draftId: 7,
      sshKey: '/home/alex/.ssh/id_ed25519.pub',
      region: 'nbg1',
      tier: 'solo',
      serverType: 'cx22',
    });
    expect(Object.keys(args ?? {})).not.toContain('token');
  });

  test('the toast says when the CLI default moved', () => {
    expect(addedMessage(targetAdded({ name: 'lab', cliDefault: { from: null, to: 'lab' } }))).toBe(
      'Target “lab” saved. Your CLI default is now lab.',
    );
    expect(addedMessage(targetAdded({ name: 'lab', cliDefault: null }))).toBe(
      'Target “lab” saved.',
    );
  });
});
