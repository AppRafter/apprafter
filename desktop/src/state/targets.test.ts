// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { expect, test } from 'bun:test';
import { targetSummary } from '../test/fixtures';
import { storeTargetNames } from './targets';

test('the store’s targets: readable and unreadable, by name, sorted as the core lists them (WI-476)', () => {
  expect(
    storeTargetNames({
      targets: [targetSummary({ name: 'staging' }), targetSummary({ name: 'lab' })],
      unreadable: [
        {
          name: 'broken',
          error: { code: null, message: 'unreadable', help: null, causes: [], fields: {} },
        },
      ],
      cliDefault: { status: 'unset' },
    }),
  ).toEqual(['broken', 'lab', 'staging']);
});
