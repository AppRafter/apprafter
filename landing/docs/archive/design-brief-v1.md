# AppRafter Landing — Design Brief

> **For:** Claude Design (Anthropic Labs)
> **Output:** Visual prototype with brand-consistent design, ready for handoff to Claude Code
> **Companion document:** `LANDING_IMPLEMENTATION_BRIEF.md` (technical implementation, not your concern)

---

## 0. Project context

**AppRafter** is an opinionated open-source PaaS (Platform-as-a-Service) built on top of Kubernetes. It targets the gap between vendor-locked PaaS (Fly.io, Railway, Render) and vanilla Kubernetes complexity. Users deploy their applications via simple CUE manifests, and the platform scales from a single €5 VPS up to confidential bare metal — all with the same developer-facing API.

The project is **pre-MVP, in active development**. Both self-hosted and managed offerings are "coming soon".

This landing page exists for:
- Communicating the project's vision and positioning
- Showing it to friends and early technical reviewers
- Visual presence (helps maintain author motivation)
- Pre-launch awareness

It is **not** a marketing funnel. No waitlist, no signups, no e-commerce.

---

## 1. Audience and tone

### Audience

**Primary:** solo founders and small-team technical leads with platform/DevOps experience. They have shipped products before. They've used Kubernetes and found it too heavy, or used Fly.io/Render and felt locked in. They appreciate technical depth and dislike marketing fluff.

**Secondary:** experienced backend engineers evaluating self-hosting options. They will skim, then dive deep on the spec if interested.

### Tone

**Serious, industrial, technical.** Not playful, not "hipster startup", not enterprise-corporate.

References for tone:
- **Tailscale's marketing site** (technical, confident, no-nonsense)
- **Linear's early site** (clean, minimal, trusted dev tool)
- **Cloudflare's product pages** (specifications front and center)

Anti-references (avoid these vibes):
- Vercel's marketing (too consumer-polished, gradient-heavy)
- Notion's site (too soft, too rounded)
- Any startup with hand-drawn illustrations or "🚀" emoji
- Any "AI for X" landing template

---

## 2. Brand identity

### Logo

The brand mark is a stylized stepped platform viewed at its corner from ground level. It conveys: foundation, layered scaling, structural support, technical precision. Variations:

1. **Two-tone primary** — dark slate mark with teal accent on top tier (default for both light and dark themes)
2. **Horizontal solid** — single-color mark when teal is contextually unavailable
3. **Monochrome** — for single-color contexts
4. **Vertical lockup** — mark above wordmark, for square containers
5. **Favicon 16×16** — simplified silhouette

Logo files will be provided as SVG.

### Wordmark

Typeset as **AppRafter** with no spaces. Two-tone variant tints "Rafter" in the accent color (teal) for added brand recognition. The wordmark must use **Roboto** typeface.

### Color palette

**Light theme:**
- Background: `#fafafa` (near-white, slight warmth)
- Surface (cards, elevated): `#ffffff`
- Foreground (primary text): `#0f172a` (dark slate)
- Muted foreground: `#64748b` (medium slate)
- Accent (logo, links, key emphases): `#14b8a6` (teal)
- Border / divider: `#e2e8f0`

**Dark theme:**
- Background: `#0a0e1a` (deep navy)
- Surface: `#111827` (slate-900)
- Foreground: `#f1f5f9` (near-white)
- Muted foreground: `#94a3b8`
- Accent: `#14b8a6` (same teal — works on both)
- Border / divider: `#1e293b`

### Typography

- **All text:** Roboto. Weights used: Regular (400), Medium (500), Bold (700).
- Headings — Bold; body — Regular; emphasis — Medium.
- Code samples — `Roboto Mono` (or system-ui mono fallback).

Justification for Roboto: it's industrial, utilitarian, neutral. Fits the platform's positioning — a serious tool, not a hipster startup. The opposite of "yet another Inter/Geist site".

### Visual language

- **Sharp edges**, no rounded corners on cards or containers (small radius 4px max for buttons / inputs only)
- **Generous spacing**, plenty of whitespace
- **Grid-based**, predictable rhythm
- **No gradients** for primary surfaces (gradient may be acceptable as small accent in hero only, if it fits)
- **No shadow-heavy** elevations — flat or single subtle shadow
- **Geometric shapes** for any decorative elements
- **No illustrations**, no character mascots, no hand-drawn anything
- **Code snippets** are first-class visual elements, treated like product screenshots

---

## 3. Theme behavior

- **Light and dark themes** must both be designed
- Default: respect OS preference via `prefers-color-scheme`
- Fallback if undetectable: **dark theme**
- Manual toggle in header (icon-only, sun/moon switch)

Both themes must be equally polished, not "dark theme as afterthought". Many target users default to dark mode.

---

## 4. Page structure

Single-page landing initially, with footer links to future legal/blog pages.

### Section order (top to bottom)

#### 4.1 Header (sticky)

- Logo (horizontal lockup, left-aligned)
- Right-aligned nav: `Spec` (links to GitHub spec), `Docs` (placeholder, "Soon"), `GitHub` (icon link, opens repo when public), theme toggle
- Subtle border-bottom on scroll

#### 4.2 Hero

- Wordmark logo prominent (or the mark + headline, designer's choice)
- **Headline:** crisp one-liner. Suggested: "An opinionated platform for the solo founder who'll outgrow it."
    - Alternatives to explore: "Kubernetes, finally usable."  /  "Deploy applications, not YAML."  /  "Self-hosted PaaS for solo founders to enterprises."
    - Final wording is editable later via CMS — design should accommodate ~6-12 words
- **Subheadline:** 1-2 sentences expanding the headline. Mentions: opinionated, vertical scaling, no vendor lock-in.
- **Primary CTA:** "Read the spec" (links to GitHub spec doc)
- **Secondary CTA:** "View on GitHub" (links to repo, may be disabled/coming soon initially)
- **Status badge:** subtle "Pre-MVP · in active development" indicator near top
- Optional: small code snippet preview (a CUE Application manifest) as visual hero element — shows what using the platform looks like

#### 4.3 What it is (positioning)

3-column or 3-row layout describing positioning:

- **For solo founders** — start on a €5 VPS, scale to enterprise without rewriting
- **No vendor lock-in** — fully open source, runs on your hardware or any cloud
- **Opinionated, not generic** — one right way to do things, fewer decisions to make

Each block: short headline + 2-3 sentences.

#### 4.4 How it works (technical depth)

This section is for the technical audience. Should feel like product documentation, not marketing.

Suggested format: split into 3-4 capability blocks, each with a code sample or diagram:

- **Application manifest** — show a CUE manifest example (`kind: Application`, with `needs.pg`, `needs.jetstream`, etc.)
- **Vertical scaling tiers** — visualize Tier 1 (single VPS) → Tier 4 (confidential bare metal). Could be a simple horizontal scale diagram.
- **Platform services** — six core multi-tenant services (Postgres, JetStream, ClickHouse, Redis, S3, Notifications) with brief explanations
- **Migration safety** — explain MigrationPlan briefly, "destructive changes pause for explicit approval"

Code samples should be syntax-highlighted in the design (even if just visually styled, exact highlighting handled by Code at implementation time).

#### 4.5 Self-hosted vs Managed

Two-column comparison:

- **Self-hosted (free, FSL-1.1-MIT):** "Runs on your infrastructure. From a single VPS to bare metal. Full control, no vendor relationship."
    - Status: "Coming soon"
    - CTA: "Read the spec"

- **Managed (paid, by us):** "We run AppRafter for you. Same architecture, same manifests, same portability. Skip the operations, keep the freedom."
    - Status: "Coming soon"
    - CTA: disabled or "Notify when available" (without an actual form — text only)

Visual emphasis: equal weight to both. Neither should look "primary".

#### 4.6 Stack & philosophy (optional, designer's call)

If the page feels light, add a section listing core technical decisions. This signals technical seriousness and helps the audience evaluate fit. Examples:

- Built on Kubernetes (k3s / Talos), Cilium, NATS JetStream, OpenBao, Backstage
- Manifests in CUE (typed, deterministic)
- Custom Rust operator (kube-rs)
- FSL-1.1-MIT licensed (auto-converts to MIT after 2 years)

Format: just a list with brief inline explanations. Not heavy graphics.

#### 4.7 Footer

- Logo (small)
- Columns:
    - **Project**: Spec, GitHub, Roadmap (placeholder)
    - **Legal**: License, Privacy, Terms (placeholders for pages to be added)
    - **Author**: link to creator's site / GitHub
- Bottom row: copyright, license note (`FSL-1.1-MIT`), small "Built on Earth" or similar low-key tagline

---

## 5. Content blocks editable via CMS

For Claude Code's later integration with Payload CMS, the following content should be marked as editable. **Do not implement the CMS** — just structure the design so these are clearly the variable parts:

- Hero headline (string, ~6-12 words)
- Hero subheadline (1-2 sentences)
- "What it is" 3 blocks (each: title + body)
- "How it works" 3-4 blocks (each: title + body + optional code sample)
- Self-hosted block copy (status, description, CTA label)
- Managed block copy (status, description, CTA label)
- Stack list items (each: technology name + one-line description)
- Footer column links

Everything else (layout, visuals, colors, typography) is design-fixed.

---

## 6. Internationalization readiness

Design must accommodate:

- Variable string lengths (English baseline, but German / Russian translations may be 30-50% longer)
- Avoid layouts that depend on exact character counts
- No flag icons in language switcher (use language codes or names instead)
- LTR only — RTL not in scope

Language switcher in header (next to theme toggle) — for now shows `EN` only with disabled dropdown. Design must show what the dropdown will look like when 2-3 languages are added later.

---

## 7. Deliverables

From Claude Design:

1. **Light theme + dark theme** of the full landing page
2. **Mobile responsive** version (single column, stacked sections, hamburger menu in header)
3. **All editable text** clearly placeholder-styled or labeled
4. **Code samples** as visual elements with monospace font and subtle background distinction
5. **Handoff bundle** for Claude Code (instructions on layout, components, and design tokens)

### Specific requests for handoff

When generating handoff for Claude Code, please include:

- **CSS custom properties** for all colors (light + dark tokens) — Code will map these to theme switching
- **Typography scale** as variables (e.g. `--text-hero`, `--text-section-title`, `--text-body`) — not raw px values
- **Spacing scale** as variables (`--space-1` through `--space-8` or similar)
- **Component structure** identified (Hero, FeatureBlock, ComparisonCard, CodeSample, etc.) — Code will turn these into Astro/Svelte components
- **Identified slots for CMS-editable content** — Code will wire these to Payload collections

---

## 8. Anti-patterns to avoid

- "Get Started" sections with fake signup forms
- Testimonials (we have no users yet, fakes are obvious)
- Logos of "trusted by" companies (we have none)
- Stats that don't exist ("10,000 deployments", "99.99% uptime")
- Generic stock photography of "diverse teams"
- 3D illustrations of clouds, servers, or "cyber" imagery
- Animated background gradients
- Sticky chat bubble in corner
- Newsletter capture popup
- Cookie consent banner with marketing language (a minimal legally-required notice is fine, but not "We value your privacy and use 47 partners…")
- Excessive emoji (a single 🛠 or similar might be OK in subtle contexts, prefer none)

---

## 9. Reference for visual feel

If asked to describe the desired visual feel in one sentence:

> A serious technical document that happens to also be a website — clean, generous in whitespace, built for engineers who'll spend more time reading than scrolling.

Closer to a well-typeset spec document than a marketing landing. The reader should feel they're being treated as a technical peer, not a conversion target.
