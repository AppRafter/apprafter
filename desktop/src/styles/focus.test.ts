// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The keyboard focus ring is an outline, so it never replaces an element's own box-shadow: a
// focused modal panel keeps its elevation. Read from the stylesheets (happy-dom does not run
// the cascade for :focus-visible).
import { expect, test } from 'bun:test';
import { join } from 'node:path';
import { effectiveProperties } from '../test/css-vars';

const read = (name: string) => Bun.file(join(import.meta.dir, name)).text();

interface Rule {
  selector: string;
  body: string;
}

/** The innermost `selector { body }` blocks: plain rules, and the rules inside @media. */
function rules(css: string): Rule[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, '');
  return [...text.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map(([, selector = '', body = '']) => ({
    selector: selector.trim().replace(/\s+/g, ' '),
    body,
  }));
}

const sheets = async () => [
  ...rules(await read('base.css')),
  ...rules(await read('components.css')),
];

test('no focus rule touches box-shadow, so a focused element keeps its own', async () => {
  const focus = (await sheets()).filter((r) => r.selector.includes(':focus-visible'));
  expect(focus.length).toBeGreaterThan(0);
  for (const rule of focus) expect(rule.body, rule.selector).not.toContain('box-shadow');
});

test('the ring is the --focus-ring outline, and that token is an outline', async () => {
  const base = (await sheets()).find((r) => r.selector === ':focus-visible');
  expect(base?.body).toMatch(/outline:\s*var\(--focus-ring\)/);
  const ring = effectiveProperties(await read('tokens.css')).get('--focus-ring');
  expect(ring).toMatch(/^\d+px solid /);
});

test('the modal panel has its elevation shadow to keep', async () => {
  const modal = (await sheets()).find((r) => r.selector === '.modal');
  expect(modal?.body).toMatch(/box-shadow:\s*var\(--shadow\)/);
});
