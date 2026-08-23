// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

import { describe, expect, it } from 'bun:test';
import { phaseOptions } from './waitlistPhaseOptions';

describe('phaseOptions', () => {
  it('maps every non-shipped registry phase to a {label,value} option keyed by id', () => {
    const opts = phaseOptions();
    expect(opts.map((o) => o.value)).toEqual(
      expect.arrayContaining(['tier2', 'managed', 'tier3', 'tier4', 'federation']),
    );
    const tier2 = opts.find((o) => o.value === 'tier2');
    expect(tier2?.label).toContain('—');
    // shipped entries are NOT subscribe targets (nothing to wait for)
    expect(opts.some((o) => o.value === 'tier1')).toBe(false);
  });
});
