<script lang="ts">
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Two-state theme toggle (dark <-> light). Initial state comes
// from the data-theme attribute set by THEME_INIT_SCRIPT in
// BaseLayout.astro before paint. On click, flip the attribute
// and persist to localStorage.

import { onMount } from 'svelte';

let theme = $state<'dark' | 'light'>('dark');

onMount(() => {
  const current = document.documentElement.getAttribute('data-theme');
  theme = current === 'light' ? 'light' : 'dark';
});

function toggle() {
  theme = theme === 'dark' ? 'light' : 'dark';
  document.documentElement.setAttribute('data-theme', theme);
  try {
    localStorage.setItem('apprafter-theme', theme);
  } catch {
    // Private mode / blocked storage — ignore.
  }
}
</script>

<button
  class="icon-btn"
  onclick={toggle}
  aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
  title={theme === 'dark' ? 'Light theme' : 'Dark theme'}
>
  {#if theme === 'dark'}
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="4" />
      <path
        d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"
      />
    </svg>
  {:else}
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z" />
    </svg>
  {/if}
</button>

<style>
  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    background: transparent;
    border: 1px solid var(--border);
    color: var(--fg-muted);
    cursor: pointer;
    transition: all 150ms ease;
  }
  .icon-btn:hover {
    color: var(--fg);
    border-color: var(--border-strong);
  }
</style>
