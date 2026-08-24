#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Smoke-test the LIVE landing site (apprafter.dev) — not a built image.
#   scripts/landing-site-smoke.sh [base-url]
#
# WHY LIVE, NOT AN IMAGE: the Payload CMS retags landing-web:preview ->
# :prod with NO pull request (landing-promote-to-prod.yml), so a content
# regression reaches production without touching this repo. This probes
# the running origin, so a bad CMS save is caught by the scheduled run
# and the post-promote run.
#
# ASSERTIONS (all derived from the repo registry + fallback, so a copy
# change that also updates the source stays green; only a DIVERGENCE
# fails):
#   1. every internal link resolves (no 404 / soft-404);
#   2. NO github.com/AppRafter/apprafter/blob/*/docs/** link (PRES-01/04);
#   3. every FUTURE "Phase N" (N>=3) label rendered exists in phases.json;
#   4. every non-shipped registry phase named carries a chip to its
#      id-anchor #roadmap-phase-<id> (the PRES-02 assertion);
#   5. the hero status-badge string matches the tracked fallback value.
#
# Judge a sandbox-run by READING THE LOG (ok/FAILED lines + final banner),
# never the reported exit code (sandbox-run masks it). CI reads the exit.

set -euo pipefail

BASE="${1:-https://apprafter.dev}"
BASE="${BASE%/}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REGISTRY="${REPO_ROOT}/landing/web/src/data/phases.json"
HERO="${REPO_ROOT}/landing/web/src/data/fallback/landingHero.json"

failures=0
fail() { printf 'FAILED  %s\n' "$*" >&2; failures=$((failures + 1)); }
ok()   { printf 'ok      %s\n' "$*"; }

HTML="$(curl -fsSL "$BASE/" || true)"
if [ -z "$HTML" ]; then
  fail "could not fetch $BASE/ (empty body)"
  echo "" >&2; echo "FAILED: 1 assertion(s) against $BASE" >&2; exit 1
fi
ok "fetched $BASE/ ($(printf '%s' "$HTML" | wc -c | tr -d ' ') bytes)"

mapfile -t REG_LABELS < <(jq -r '.phases[].label' "$REGISTRY" | sort -u)
mapfile -t FUTURE_IDS < <(jq -r '.phases[] | select(.status != "shipped") | .id' "$REGISTRY")

# 2. No github blob/docs links (PRES-01/PRES-04 made permanent).
# NB: here-string, not `printf | grep -q` — `grep -q` exits on first
# match and SIGPIPEs printf, which under `set -o pipefail` makes the
# pipeline non-zero even ON A MATCH (a false negative that would let a
# real blob/docs link slip through). The here-string has no upstream.
if grep -Eq 'github\.com/AppRafter/apprafter/blob/[^"'"'"' ]*/docs/' <<<"$HTML"; then
  fail "page links to a github .../blob/*/docs/* URL (must use docs.apprafter.dev)"
else
  ok "no github blob/docs links on the page (PRES-01/PRES-04 held)"
fi

# 1. Every internal link resolves (no 4xx). Same-origin + apprafter.dev only.
mapfile -t LINKS < <(
  printf '%s' "$HTML" \
    | grep -oE 'href="[^"]+"' | sed -E 's/^href="//; s/"$//' \
    | grep -E '^(/|https://apprafter\.dev/)' \
    | sed -E 's#^https://apprafter\.dev##' \
    | grep -vE '^/?#' | sort -u
)
link_failures_before=$failures
for path in "${LINKS[@]}"; do
  [ -n "$path" ] || continue
  url="$BASE/${path#/}"
  code="$(curl -s -o /dev/null -w '%{http_code}' "$url")"
  if [ "$code" -ge 400 ]; then fail "internal link $path -> $code"; fi
done
if [ "$failures" -eq "$link_failures_before" ]; then
  ok "all ${#LINKS[@]} internal link(s) resolve (no 4xx)"
fi

# 3. Every FUTURE "Phase N" (N>=3) label on the page exists in phases.json.
#    Phase 0/1/2 are the shipped era (the status badge says "Phase 2
#    shipped") — not roadmap registry labels, so skip them.
mapfile -t PAGE_LABELS < <(printf '%s' "$HTML" | grep -oE 'Phase [0-9]+\+?' | sort -u)
label_failures_before=$failures
future_seen=0
for lbl in "${PAGE_LABELS[@]}"; do
  n="$(printf '%s' "$lbl" | grep -oE '[0-9]+')"
  [ "$n" -lt 3 ] && continue
  future_seen=$((future_seen + 1))
  found=0
  for reg in "${REG_LABELS[@]}"; do [ "$lbl" = "$reg" ] && found=1 && break; done
  # Tolerate an informal "Phase N+" ("N or later") when the base "Phase N"
  # IS a registry label — the Advantages copy hedges bundled/post-launch
  # items as "Phase 3+"/"Phase 4+". Still fails an unknown phase number
  # (e.g. "Phase 7", "Phase 9+"), which is real registry drift.
  if [ "$found" -eq 0 ] && [ "${lbl%+}" != "$lbl" ]; then
    base="${lbl%+}"
    for reg in "${REG_LABELS[@]}"; do [ "$base" = "$reg" ] && found=1 && break; done
  fi
  [ "$found" -eq 1 ] || fail "rendered roadmap label \"$lbl\" is not in phases.json (registry drift)"
done
if [ "$future_seen" -eq 0 ]; then
  fail "no future \"Phase N\" (N>=3) label on the page — did the roadmap stop rendering?"
elif [ "$failures" -eq "$label_failures_before" ]; then
  ok "all $future_seen future roadmap label(s) are in the registry"
fi

# 4. PRES-02: every non-shipped registry phase is represented — either a
#    chip link to its id-anchor (what <PhaseChip> renders) or the roadmap
#    section itself carries that id (e.g. Tier 4 is named by its section,
#    not chipped from an inventory item). Only a phase with NEITHER is a
#    regression.
for id in "${FUTURE_IDS[@]}"; do
  anchor="roadmap-phase-${id}"
  if grep -Eq "href=\"[^\"]*#${anchor}\"" <<<"$HTML"; then
    ok "phase '$id' is chipped (href …#${anchor} present)"
  elif grep -Eq "id=\"${anchor}\"" <<<"$HTML"; then
    ok "phase '$id' is represented by its roadmap section (#${anchor})"
  else
    fail "phase '$id' is absent — no chip and no roadmap section (#${anchor}) — PRES-02 regression"
  fi
done
if [ "${#FUTURE_IDS[@]}" -eq 0 ]; then
  fail "registry has no non-shipped phase — assertion 4 checks the empty set (is phases.json right?)"
fi

# 5. Hero status badge on the page matches the tracked fallback value.
BADGE="$(jq -r '.statusBadge' "$HERO")"
if [ -z "$BADGE" ] || [ "$BADGE" = "null" ]; then
  fail "no statusBadge in $HERO — cannot check assertion 5"
elif grep -Fq "$BADGE" <<<"$HTML"; then
  ok "hero status badge on the page matches the tracked source"
else
  fail "hero status badge on the page does NOT match \"$BADGE\" (CMS↔fallback drift)"
fi

if [ "$failures" -ne 0 ]; then
  echo "" >&2; echo "FAILED: $failures assertion(s) against $BASE" >&2; exit 1
fi
echo ""; echo "GREEN: landing-site smoke OK against $BASE"
