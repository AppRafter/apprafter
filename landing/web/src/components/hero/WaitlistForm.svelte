<script lang="ts">
// SPDX-FileCopyrightText: 2026 AppRafter contributors
// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//
// Inline managed-launch waitlist. Hidden by default; the secondary
// CTA in Hero.astro dispatches a `waitlist:toggle` CustomEvent that
// flips the open state here. POST target: Payload's auto-generated
// REST endpoint for the WaitlistSignups collection (lands in Phase
// H). In dev the Vite proxy maps /api -> the CMS port set via
// LANDING_CMS_PORT; in prod set PUBLIC_CMS_URL to the absolute
// origin of the deployed CMS.

type Props = {
  formIntro: string;
  emailLabel: string;
  useCaseLabel: string;
  interestsLabel: string;
  interests?: { key: string; label: string; id?: string | null }[] | null | undefined;
  callLabel: string;
  submitLabel: string;
  successMessage: string;
  successWithCall: string;
  storageNote: string;
};

let {
  formIntro,
  emailLabel,
  useCaseLabel,
  interestsLabel,
  interests,
  callLabel,
  submitLabel,
  successMessage,
  successWithCall,
  storageNote,
}: Props = $props();

let open = $state(false);
let email = $state('');
let useCase = $state('');
let wantsCall = $state(false);
let submitted = $state(false);
let submitting = $state(false);
let error = $state<string | null>(null);
let selected = $state<Record<string, boolean>>({});

$effect(() => {
  const onToggle = (e: Event) => {
    const detail = (e as CustomEvent<boolean | { open?: boolean; preselect?: string }>).detail;
    if (typeof detail === 'boolean') {
      open = detail;
    } else if (detail && typeof detail === 'object') {
      open = detail.open ?? true;
      if (detail.preselect) selected = { ...selected, [detail.preselect]: true };
    } else {
      open = !open;
    }
    if (open) {
      requestAnimationFrame(() =>
        document
          .getElementById('waitlist-form')
          ?.scrollIntoView({ behavior: 'smooth', block: 'center' }),
      );
    }
  };
  document.addEventListener('waitlist:toggle', onToggle);
  return () => document.removeEventListener('waitlist:toggle', onToggle);
});

const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

async function submit(ev: SubmitEvent) {
  ev.preventDefault();
  error = null;
  if (!EMAIL_RE.test(email)) {
    error = 'Please enter a valid email address.';
    return;
  }
  submitting = true;
  try {
    const base = import.meta.env.PUBLIC_CMS_URL || '';
    const interestKeys = Object.entries(selected)
      .filter(([, v]) => v)
      .map(([k]) => k);
    const res = await fetch(`${base}/api/waitlist-signups`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        email,
        useCase: useCase || undefined,
        wantsCall,
        interests: interestKeys.length ? interestKeys : undefined,
        source: typeof document !== 'undefined' ? document.referrer || 'direct' : 'direct',
      }),
    });
    if (res.ok) {
      submitted = true;
    } else if (res.status === 409 || res.status === 400) {
      // unique constraint — treat as already-signed-up success
      submitted = true;
    } else {
      error = `Submit failed (${res.status}). Try again in a minute.`;
    }
  } catch {
    error = 'Network error — try again.';
  } finally {
    submitting = false;
  }
}
</script>

{#if open}
  <div class="waitlist" id="waitlist-form" role="region" aria-label="Waitlist">
    {#if submitted}
      <div class="waitlist-success" role="status">
        {wantsCall ? successWithCall : successMessage}
      </div>
    {:else}
      <form onsubmit={submit} novalidate>
        <p class="copy">{formIntro}</p>

        <div class="field">
          <label for="wl-email">{emailLabel}</label>
          <input
            id="wl-email"
            type="email"
            required
            autocomplete="email"
            placeholder="you@company.com"
            bind:value={email}
            disabled={submitting}
          />
        </div>

        <div class="field">
          <label for="wl-use">{useCaseLabel}</label>
          <input
            id="wl-use"
            type="text"
            placeholder="Small SaaS, 3 services, currently on Render"
            bind:value={useCase}
            disabled={submitting}
          />
        </div>

        {#if interests?.length}
          <fieldset class="interests">
            <legend>{interestsLabel}</legend>
            {#each interests as it (it.key)}
              <label class="checkbox-row">
                <input type="checkbox" bind:checked={selected[it.key]} disabled={submitting} />
                <span>{it.label}</span>
              </label>
            {/each}
          </fieldset>
        {/if}

        <label class="checkbox-row">
          <input type="checkbox" bind:checked={wantsCall} disabled={submitting} />
          <span>{callLabel}</span>
        </label>

        {#if error}
          <p class="error" role="alert">{error}</p>
        {/if}

        <div class="waitlist-actions">
          <button type="submit" class="btn btn-primary" disabled={submitting}>
            {submitting ? 'Sending…' : submitLabel}
          </button>
          <span class="faint" style="font-size: 12px;">{storageNote}</span>
        </div>
      </form>
    {/if}
  </div>
{/if}

<style>
  .waitlist {
    margin-top: 24px;
    border: 1px solid var(--border-strong);
    padding: 24px;
    background: var(--surface);
    max-width: 560px;
    display: grid;
    gap: 14px;
  }
  form {
    display: grid;
    gap: 14px;
  }
  p.copy {
    font-size: 14.5px;
    color: var(--fg-muted);
    line-height: 1.55;
    margin: 0 0 6px;
  }
  .field {
    display: grid;
    gap: 6px;
  }
  .field label {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .field input[type='email'],
  .field input[type='text'] {
    background: var(--bg);
    border: 1px solid var(--border-strong);
    color: var(--fg);
    font-family: var(--font-sans);
    font-size: 15px;
    padding: 10px 12px;
    border-radius: 4px;
    outline: none;
    transition: border-color 150ms ease;
  }
  .field input:focus {
    border-color: var(--accent);
  }
  .interests {
    border: 0;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 10px;
  }
  .interests legend {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--fg-faint);
    padding: 0;
    margin-bottom: 4px;
  }
  .checkbox-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    font-size: 14px;
    color: var(--fg-muted);
    cursor: pointer;
    user-select: none;
  }
  .checkbox-row input {
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .error {
    margin: 0;
    font-size: 13px;
    color: #f87171;
  }
  .waitlist-actions {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 4px;
    flex-wrap: wrap;
  }
  .waitlist-success {
    font-family: var(--font-mono);
    font-size: 13px;
    color: var(--accent);
    padding: 12px 14px;
    border: 1px solid var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, var(--surface));
  }
  .faint {
    color: var(--fg-faint);
  }
  /* Submit button — same .btn / .btn-primary palette as Hero CTAs.
     Duplicated here because Svelte scoped styles can't reach across
     the island boundary. */
  .btn {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 44px;
    padding: 0 20px;
    font-family: var(--font-sans);
    font-size: 15px;
    font-weight: 500;
    letter-spacing: -0.005em;
    border-radius: 4px;
    border: 1px solid transparent;
    cursor: pointer;
    transition: all 150ms ease;
    white-space: nowrap;
  }
  .btn-primary {
    background: var(--accent);
    color: var(--accent-fg);
    border-color: var(--accent);
  }
  .btn-primary:hover {
    background: var(--accent-2);
    border-color: var(--accent-2);
  }
  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
</style>
