// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { catalogue, offer } from '../../test/flows';
import {
  choosable,
  defaultRegion,
  defaultSku,
  formatAmount,
  formatGb,
  formatPrice,
  machineSummary,
  offerIn,
  offerNote,
  regionChips,
  visibleOffers,
} from './catalogue';

describe('regionChips', () => {
  test('regions with an offer, nearest first, those without an answer after, then by code', () => {
    const chips = regionChips(catalogue(), [
      { region: 'nbg1', latencyMs: 38 },
      { region: 'hel1', latencyMs: 12 },
      { region: 'fsn1', latencyMs: null },
    ]);
    expect(chips.map((c) => [c.value, c.secondary, c.meta])).toEqual([
      ['hel1', 'Helsinki', '12 ms'],
      ['nbg1', 'Nuremberg', '38 ms'],
      ['fsn1', 'Falkenstein', 'no answer'],
    ]);
  });

  test('a region the probes did not cover shows a dash, not "no answer"', () => {
    const chips = regionChips(catalogue(), [{ region: 'nbg1', latencyMs: 38 }]);
    expect(chips.map((c) => [c.value, c.meta])).toEqual([
      ['nbg1', '38 ms'],
      ['fsn1', '–'],
      ['hel1', '–'],
    ]);
  });

  test('while measuring, every chip says so, in code order whatever order the regions came in', () => {
    const cat = catalogue();
    cat.regions.reverse();
    const chips = regionChips(cat, null);
    expect(chips.map((c) => c.value)).toEqual(['fsn1', 'hel1', 'nbg1']);
    expect(chips.every((c) => c.meta === '…')).toBe(true);
  });

  test('not measured (the reading failed or was cancelled): no latency at all, in code order', () => {
    const cat = catalogue();
    cat.regions.reverse();
    const chips = regionChips(cat, [{ region: 'nbg1', latencyMs: 38 }], false);
    expect(chips.map((c) => [c.value, c.meta])).toEqual([
      ['fsn1', undefined],
      ['hel1', undefined],
      ['nbg1', undefined],
    ]);
  });
});

describe('offers', () => {
  const all = { arch: 'all', cpu: 'all', hideUnavailable: false } as const;

  test("a region's offers only; filters by arch and CPU type", () => {
    expect(visibleOffers(catalogue(), 'hel1', all).rows.map((o) => o.sku)).toEqual(['cx22']);
    expect(
      visibleOffers(catalogue(), 'nbg1', { ...all, arch: 'arm' }).rows.map((o) => o.sku),
    ).toEqual(['cax11']);
    expect(
      visibleOffers(catalogue(), 'nbg1', { ...all, cpu: 'dedicated' }).rows.map((o) => o.sku),
    ).toEqual(['ccx13']);
  });

  test('shown, nothing is hidden; hiding the unavailable counts what it hid', () => {
    expect(visibleOffers(catalogue(), 'nbg1', all).hidden).toBe(0);
    const shown = visibleOffers(catalogue(), 'nbg1', { ...all, hideUnavailable: true });
    expect(shown.rows.map((o) => o.sku)).toEqual(['cx22', 'cpx22', 'cax11', 'ccx13', 'cpx11']);
    expect(shown.hidden).toBe(2);
  });

  test('notes: retired, sold out, retiring with its date, recommended', () => {
    expect(offerNote(offer({ retired: true, available: false }))).toEqual({
      text: 'retired',
      tone: 'faint',
    });
    expect(offerNote(offer({ available: false }))).toEqual({
      text: 'sold out here',
      tone: 'faint',
    });
    expect(
      offerNote(
        offer({ deprecation: { announced: null, unavailableAfter: '2026-12-01T00:00:00+00:00' } }),
      ),
    ).toEqual({ text: 'retiring after 2026-12-01', tone: 'warn' });
    expect(offerNote(offer({ deprecation: { announced: null, unavailableAfter: null } }))).toEqual({
      text: 'retiring',
      tone: 'warn',
    });
    expect(offerNote(offer({ recommended: true }))).toEqual({
      text: 'recommended',
      tone: 'accent',
    });
    expect(offerNote(offer())).toBeNull();
  });

  test('a retiring offer can still be chosen; sold out and retired cannot', () => {
    expect(choosable(offer({ deprecation: { announced: null, unavailableAfter: null } }))).toBe(
      true,
    );
    expect(choosable(offer({ available: false }))).toBe(false);
    // The core's check refuses a retired type even where the provider still lists it available.
    expect(choosable(offer({ retired: true }))).toBe(false);
  });
});

describe('defaults and text', () => {
  test("nbg1 first (the CLI's default region), else the first region with an offer", () => {
    expect(defaultRegion(catalogue())).toBe('nbg1');
    const elsewhere = catalogue();
    elsewhere.offers = elsewhere.offers.filter((o) => o.location !== 'nbg1');
    expect(defaultRegion(elsewhere)).toBe('fsn1');
    expect(defaultRegion({ regions: elsewhere.regions, offers: [] })).toBeNull();
  });

  test("the region's recommended choosable offer, else none", () => {
    expect(defaultSku(catalogue(), 'nbg1')).toBe('cx22');
    expect(defaultSku(catalogue(), 'fsn1')).toBeNull();
    const soldOut = catalogue();
    soldOut.offers = [offer({ recommended: true, available: false })];
    expect(defaultSku(soldOut, 'nbg1')).toBeNull();
  });

  test("an offer is a region's: the same type elsewhere is not it", () => {
    expect(offerIn(catalogue(), 'fsn1', 'cpx22')?.location).toBe('fsn1');
    expect(offerIn(catalogue(), 'hel1', 'cpx22')).toBeUndefined();
    expect(offerIn(catalogue(), 'nbg1', null)).toBeUndefined();
  });

  test('prices: net, two or four decimals; a missing or unreadable price is a dash', () => {
    expect(formatPrice('3.7900000000', 2)).toBe('€3.79');
    expect(formatPrice('0.0060000000', 4)).toBe('€0.0060');
    expect(formatPrice(null, 2)).toBe('–');
    expect(formatAmount('7.5500000000', 2)).toBe('7.55');
    expect(formatAmount('0.0121000000', 4)).toBe('0.0121');
    expect(formatAmount('n/a', 2)).toBe('–');
  });

  test('memory: whole gigabytes plain, a fraction to one place', () => {
    expect(formatGb(4)).toBe('4');
    expect(formatGb(0.5)).toBe('0.5');
  });

  test('summary: one server, excl. VAT; an unavailable choice says to pick another', () => {
    expect(machineSummary(offer({ sku: 'cx22' }), 'nbg1')).toBe(
      'cx22 · 2 vCPU · 4 GB RAM · 40 GB SSD · €3.79 / mo, excl. VAT · one server',
    );
    expect(machineSummary(offer({ priceMonthlyNet: null }), 'nbg1')).toBe(
      'cx22 · 2 vCPU · 4 GB RAM · 40 GB SSD · price not listed · one server',
    );
    expect(machineSummary(offer({ sku: 'cx32', available: false }), 'nbg1')).toBe(
      'cx32 is sold out in nbg1: pick another machine.',
    );
    expect(machineSummary(offer({ sku: 'cx11', retired: true }), 'nbg1')).toBe(
      'cx11 is retired: pick another machine.',
    );
    expect(machineSummary(undefined, 'nbg1')).toBe('Pick a machine.');
  });
});
