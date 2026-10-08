// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import { effectiveProperties, normalizeValue } from './css-vars';

const props = (css: string, theme?: string) => Object.fromEntries(effectiveProperties(css, theme));

describe('effectiveProperties', () => {
  test('reads the custom properties of :root, skipping comments and ordinary properties', () => {
    const css = `/* header */ :root { color-scheme: dark; --bg: #000; /* note */ --fg:#fff; }`;
    expect(props(css)).toEqual({ '--bg': '#000', '--fg': '#fff' });
  });

  test('a theme block overrides what it declares and inherits the rest from :root', () => {
    const css = `:root { --bg: #000; --accent: #0aa; } [data-theme='light'] { --bg: #fff; }`;
    expect(props(css, 'light')).toEqual({ '--bg': '#fff', '--accent': '#0aa' });
    expect(props(css, 'dark')).toEqual({ '--bg': '#000', '--accent': '#0aa' });
    expect(props(css)).toEqual({ '--bg': '#000', '--accent': '#0aa' });
  });

  test(':root[data-theme] outranks :root wherever it stands', () => {
    const css = `:root[data-theme="light"] { --bg: #fff; } :root { --bg: #000; }`;
    expect(props(css, 'light')).toEqual({ '--bg': '#fff' });
  });

  test('at equal specificity the later rule wins', () => {
    const css = `[data-theme=light] { --bg: #fff; } :root { --bg: #000; }`;
    expect(props(css, 'light')).toEqual({ '--bg': '#000' });
  });

  test('a selector list applies to each of its selectors', () => {
    const css = `:root, :root[data-theme='dark'] { --bg: #000; } :root[data-theme='light'] { --bg: #fff; }`;
    expect(props(css)).toEqual({ '--bg': '#000' });
    expect(props(css, 'dark')).toEqual({ '--bg': '#000' });
    expect(props(css, 'light')).toEqual({ '--bg': '#fff' });
  });

  test('values keep their parentheses, quotes and commas', () => {
    const css = `:root { --font: 'Roboto', system-ui;
      --shadow: 0 24px  64px rgba(0, 0, 0, 0.55); --ring: 0 0 0 2px var(--accent) }`;
    expect(props(css)).toEqual({
      '--font': "'Roboto',system-ui",
      '--shadow': '0 24px 64px rgba(0,0,0,0.55)',
      '--ring': '0 0 0 2px var(--accent)',
    });
  });

  test('refuses what it cannot model instead of skipping it', () => {
    expect(() => effectiveProperties('@media (x) { :root { --bg: #000; } }')).toThrow('at-rule');
    expect(() => effectiveProperties('html { --bg: #000; }')).toThrow('selector');
    expect(() => effectiveProperties(':root { --bg: #000 !important; }')).toThrow('!important');
    expect(() => effectiveProperties(':root { --bg: #000; ')).toThrow('unclosed');
    expect(() => effectiveProperties(':root { a { --bg: #000; } }')).toThrow('nested');
    expect(() => effectiveProperties(':root { --bg #000; }')).toThrow('declaration');
  });
});

test('normalizeValue collapses whitespace and drops it around commas', () => {
  expect(normalizeValue('  rgba( 20,184, 166 ,0.12 )\n')).toBe('rgba(20,184,166,0.12)');
  expect(normalizeValue('0  24px\t64px')).toBe('0 24px 64px');
});

// Biome's CSS formatter prints `0.10` as `0.1`; the design file keeps `0.10`.
test('normalizeValue drops trailing decimal zeros and leaves hex colours alone', () => {
  expect(normalizeValue('rgba(20, 184, 166, 0.10)')).toBe('rgba(20,184,166,0.1)');
  expect(normalizeValue('2.0px 10.50px 0.05')).toBe('2px 10.5px 0.05');
  expect(normalizeValue('#0f7600 #100')).toBe('#0f7600 #100');
});
