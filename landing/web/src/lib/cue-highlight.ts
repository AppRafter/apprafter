// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0

/**
 * Build-time CUE syntax highlighter. Ports the naive tokenizer from
 * landing/docs/design-source/sections.jsx (renderCue, ll. 91-122)
 * into a pure function returning sanitized HTML.
 *
 * Output goes into `<pre set:html={cueTokenize(snippet)} />` inside
 * HeroCodeBlock.astro — server-rendered once at build time, zero
 * JavaScript shipped to the browser for highlighting. The
 * accompanying styles (.tok-*) live in HeroCodeBlock.astro's scoped
 * CSS, matching the palette in design-source/styles.css.
 *
 * The tokenizer is intentionally coarse — it's good enough for the
 * one CUE manifest we show in hero, not a full parser. Keep it
 * simple; if we ever ship a richer code block (Helm, K8s YAML)
 * we'll reach for Shiki with a CUE grammar instead.
 */

// Regex captures, in priority order:
//   1: double-quoted string
//   2: number
//   3: structural CUE keyword
//   4: punctuation / single-char operator
//   5: identifier
const TOKEN_RE =
  /("(?:[^"\\]|\\.)*")|(\b\d+\b)|(\b(?:apiVersion|kind|metadata|spec|base|environments|expose|env|image|replicas|port|public|network|name|namespace|needs|from|claim|size)\b)|(&|\||\?|:|\.|\{|\}|\[|\])|([A-Za-z_][A-Za-z0-9_-]*)/g;

function esc(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

export function cueTokenize(src: string): string {
  return src
    .split('\n')
    .map((line) => {
      const leadMatch = line.match(/^(\s+)/);
      const lead = leadMatch?.[1] ?? '';
      const rest = lead ? line.slice(lead.length) : line;

      if (rest.startsWith('//')) {
        return `<div>${esc(lead)}<span class="tok-cmt">${esc(rest)}</span></div>`;
      }

      let out = esc(lead);
      let last = 0;
      for (const m of rest.matchAll(TOKEN_RE)) {
        const idx = m.index ?? 0;
        if (idx > last) out += esc(rest.slice(last, idx));
        if (m[1]) out += `<span class="tok-str">${esc(m[1])}</span>`;
        else if (m[2]) out += `<span class="tok-num">${esc(m[2])}</span>`;
        else if (m[3]) out += `<span class="tok-key">${esc(m[3])}</span>`;
        else if (m[4]) out += `<span class="tok-kw">${esc(m[4])}</span>`;
        else if (m[5]) out += `<span class="tok-ident">${esc(m[5])}</span>`;
        last = idx + m[0].length;
      }
      if (last < rest.length) out += esc(rest.slice(last));

      return `<div style="min-height:1.65em">${out || '&nbsp;'}</div>`;
    })
    .join('');
}
