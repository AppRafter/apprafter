// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Reads the custom properties a token stylesheet gives <html>, per `data-theme` value — enough
// cascade to compare two token files, and it throws on anything it does not model (at-rules,
// other selectors, nesting, !important) rather than skip it.

interface Rule {
  readonly selectors: readonly string[];
  readonly declarations: ReadonlyMap<string, string>;
}

/**
 * Whitespace collapsed to one space, none around commas or inside parentheses, and no trailing
 * decimal zeros: `rgba(0, 0, 0, 0.10)` equals `rgba(0,0,0,0.1)`.
 */
export function normalizeValue(value: string): string {
  return value
    .replace(/\s+/g, ' ')
    .replace(/\s*,\s*/g, ',')
    .replace(/\(\s*/g, '(')
    .replace(/\s*\)/g, ')')
    .replace(/(?<![#\w.])(\d+)\.(\d*?)0+(?!\d)/g, (_, whole, fraction) =>
      fraction === '' ? whole : `${whole}.${fraction}`,
    )
    .trim();
}

function splitOutside(text: string, separator: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let quote: string | undefined;
  let start = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quote !== undefined) {
      if (c === quote) quote = undefined;
    } else if (c === "'" || c === '"') {
      quote = c;
    } else if (c === '(') {
      depth++;
    } else if (c === ')') {
      depth--;
    } else if (c === separator && depth === 0) {
      parts.push(text.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(text.slice(start));
  return parts;
}

function parseRules(css: string): Rule[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, '');
  const rules: Rule[] = [];
  let at = 0;
  while (text.slice(at).trim() !== '') {
    const open = text.indexOf('{', at);
    const close = text.indexOf('}', at);
    if (open === -1 || (close !== -1 && close < open))
      throw new Error('a block without a selector');
    const prelude = text.slice(at, open).trim();
    if (prelude.startsWith('@')) throw new Error(`an at-rule is not modelled: ${prelude}`);
    const end = text.indexOf('}', open);
    if (end === -1) throw new Error(`an unclosed block: ${prelude}`);
    const body = text.slice(open + 1, end);
    if (body.includes('{')) throw new Error(`a nested block in ${prelude}`);
    const declarations = new Map<string, string>();
    for (const raw of splitOutside(body, ';')) {
      if (raw.trim() === '') continue;
      const colon = raw.indexOf(':');
      if (colon === -1) throw new Error(`not a declaration: ${raw.trim()}`);
      const name = raw.slice(0, colon).trim();
      const value = raw.slice(colon + 1);
      if (value.includes('!important')) throw new Error(`!important is not modelled: ${name}`);
      if (name.startsWith('--')) declarations.set(name, normalizeValue(value));
    }
    const selectors = splitOutside(prelude, ',').map((s) => s.trim());
    rules.push({ selectors, declarations });
    at = end + 1;
  }
  return rules;
}

const THEMED = /^(:root)?\[data-theme=(['"]?)([\w-]+)\2\]$/;

/** The selector's specificity if it matches <html data-theme={theme}>, else undefined. */
function matches(selector: string, theme: string | undefined): number | undefined {
  const compact = selector.replace(/\s+/g, '');
  if (compact === ':root') return 1;
  const themed = compact.match(THEMED);
  if (themed === null) throw new Error(`a selector that is not modelled: ${selector}`);
  if (themed[3] !== theme) return undefined;
  return themed[1] === undefined ? 1 : 2;
}

/**
 * The custom properties <html> ends up with when its `data-theme` is `theme` (undefined: no
 * attribute): every matching rule applied by specificity, then source order.
 */
export function effectiveProperties(css: string, theme?: string): Map<string, string> {
  const applicable: { specificity: number; order: number; rule: Rule }[] = [];
  parseRules(css).forEach((rule, order) => {
    const specificities = rule.selectors
      .map((s) => matches(s, theme))
      .filter((s): s is number => s !== undefined);
    if (specificities.length > 0) {
      applicable.push({ specificity: Math.max(...specificities), order, rule });
    }
  });
  applicable.sort((a, b) => a.specificity - b.specificity || a.order - b.order);
  const effective = new Map<string, string>();
  for (const { rule } of applicable) {
    for (const [name, value] of rule.declarations) effective.set(name, value);
  }
  return effective;
}
