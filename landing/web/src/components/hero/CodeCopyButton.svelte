<script lang="ts">
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Copy-to-clipboard for the hero CUE snippet. The only interactive
// piece of HeroCodeBlock — the tokenized markup itself ships as
// pre-rendered HTML from cueTokenize() at build time.

type Props = { snippet: string };

let { snippet }: Props = $props();
let copied = $state(false);
let timer: ReturnType<typeof setTimeout> | null = null;

async function copy() {
  try {
    await navigator.clipboard.writeText(snippet);
    copied = true;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      copied = false;
    }, 1500);
  } catch {
    // Older browsers / iframe contexts may block clipboard — silent.
  }
}
</script>

<button
  class={'copy-btn' + (copied ? ' copied' : '')}
  onclick={copy}
  aria-live="polite"
  type="button"
>
  {#if copied}
    <svg
      width="11"
      height="11"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="3"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <polyline points="20 6 9 17 4 12" />
    </svg>
    Copied
  {:else}
    <svg
      width="11"
      height="11"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <rect x="9" y="9" width="13" height="13" rx="1" />
      <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
    </svg>
    Copy
  {/if}
</button>

<style>
  .copy-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: 1px solid #1e293b;
    color: #94a3b8;
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    padding: 5px 9px;
    cursor: pointer;
    transition: all 150ms ease;
  }
  .copy-btn:hover {
    color: #e2e8f0;
    border-color: #334155;
  }
  .copy-btn.copied {
    color: var(--accent);
    border-color: var(--accent);
  }
</style>
