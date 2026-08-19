#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Smoke-test a built docs image before it is pushed.
#
#   scripts/docs-site-smoke.sh <image-ref>
#
# WHY THIS EXISTS. Until it did, nothing in this repository read
# `docs-site/Caddyfile`: `just lint` does not, `scripts/docs-check.sh`
# never looks below `docs/`, and the drift gate's corpus excludes
# `docs-site/` by construction. The image's own build now runs
# `caddy validate` (docs-site/Dockerfile), which catches a config the
# server cannot LOAD — and nothing more. A config that loads perfectly
# and serves HTML pinned `immutable` for a year, or hands a reader the
# 40 KB HTML error page relabelled `text/plain`, is a green build.
# Both of those were published and measured green before this script.
#
# So this checks the one thing `caddy validate` cannot: that the
# running container ANSWERS correctly.
#
# WHAT IT DOES NOT DO. It asserts response STATUS and CONTENT TYPE, not
# page content — the pages are the documentation gate's business
# (`scripts/docs-check.sh`), and re-checking them here would be a
# second, weaker copy of that gate.
#
# The probed URLs are DERIVED FROM THE IMAGE'S OWN FILE TREE rather
# than hardcoded, so a page rename cannot silently turn a probe into a
# request for something that was never there — which would pass the
# 404 probes and vacuously "pass" the rest. The expectations are not
# derived from the same place: the tree says WHICH URLs exist, the
# Caddyfile decides HOW they are answered, and only the second is under
# test here.
#
# Prove it fires before trusting it. An image built from a Caddyfile
# whose `handle_errors` does not split the markdown case — the shape
# this repository shipped until the missing-twin probe was added —
# fails probe 6 and exits 1.

set -euo pipefail

IMAGE="${1:-}"
if [ -z "$IMAGE" ]; then
	echo "usage: $0 <image-ref>" >&2
	exit 2
fi

PORT="${DOCS_SMOKE_PORT:-18099}"
BASE="http://127.0.0.1:${PORT}"
CID=""
failures=0

cleanup() {
	if [ -n "$CID" ]; then
		docker rm -f "$CID" >/dev/null 2>&1 || true
	fi
}
trap cleanup EXIT

echo "==> starting $IMAGE on :$PORT"
CID=$(docker run -d -p "${PORT}:80" "$IMAGE")

# Wait for the server rather than sleeping a guessed interval: a fixed
# sleep either wastes time or fails on a slow runner.
ready=0
for _ in $(seq 1 60); do
	if curl -fsS -o /dev/null "${BASE}/" 2>/dev/null; then
		ready=1
		break
	fi
	sleep 0.5
done
if [ "$ready" -ne 1 ]; then
	echo "FAILED: container never answered ${BASE}/ within 30s" >&2
	docker logs "$CID" 2>&1 | tail -30 >&2
	exit 1
fi

# ---------------------------------------------------------------------
# Derive the page/twin pair from the tree baked into the image.
# ---------------------------------------------------------------------
twin_abs=$(docker exec "$CID" sh -c \
	"find /usr/share/caddy -type f -name '*.md' | sort | head -n1")
twin_count=$(docker exec "$CID" sh -c \
	"find /usr/share/caddy -type f -name '*.md' | wc -l" | tr -d '[:space:]')

if [ -z "$twin_abs" ] || [ "$twin_count" -lt 1 ]; then
	echo "FAILED: the image contains no markdown twin, so probes 3 and 4" >&2
	echo "        would be vacuous. Has the llm_export hook stopped running?" >&2
	exit 1
fi

TWIN_URL="${twin_abs#/usr/share/caddy}"
PAGE_URL="${TWIN_URL%.md}/"

# The twin must actually have the page it is a twin OF, or probe 3 is a
# 404 dressed up as a routing check.
if ! docker exec "$CID" test -f "/usr/share/caddy${PAGE_URL}index.html"; then
	echo "FAILED: derived twin $TWIN_URL has no page at ${PAGE_URL}index.html" >&2
	exit 1
fi

echo "==> $twin_count twin(s) in the image; probing the pair $PAGE_URL / $TWIN_URL"

# A path that cannot exist, so the 404 probes test routing rather than a
# page someone happened to delete.
MISS="/no-such-path-$(date +%s)-smoke"

# ---------------------------------------------------------------------
# probe <path> <expected-status> <expected-content-type-substring> [max-bytes]
# ---------------------------------------------------------------------
probe() {
	local path="$1" want_status="$2" want_ct="$3" max_bytes="${4:-}"
	local out status ct bytes
	# `|`-separated, NOT space-separated: content_type carries a space
	# ("text/plain; charset=utf-8"), so splitting on space put
	# `charset=utf-8` in the size field and made the byte bound below a
	# no-op. Caught by the bound not firing on an image it should have.
	out=$(curl -s -o /dev/null -w '%{http_code}|%{content_type}|%{size_download}' "${BASE}${path}")
	status=${out%%|*}
	ct=${out#*|}
	ct=${ct%|*}
	bytes=${out##*|}

	local bad=""
	[ "$status" = "$want_status" ] || bad="status=$status want=$want_status"
	case "$ct" in
	*"$want_ct"*) ;;
	*) bad="$bad ct=$ct want~=$want_ct" ;;
	esac
	if [ -n "$max_bytes" ] && [ "$bytes" -gt "$max_bytes" ]; then
		bad="$bad bytes=$bytes want<=$max_bytes"
	fi

	if [ -n "$bad" ]; then
		printf 'FAILED  %-44s %s\n' "$path" "$bad" >&2
		failures=$((failures + 1))
	else
		printf 'ok      %-44s %s %s (%s bytes)\n' "$path" "$status" "$ct" "$bytes"
	fi
}

probe "/" 200 "text/html"
probe "/llms.txt" 200 "text/plain"
probe "$PAGE_URL" 200 "text/html"
probe "$TWIN_URL" 200 "text/plain"

# An unknown page is a REAL 404, not the soft 200 a `/404.html` entry in
# try_files produces — that shape is what landing/web/Caddyfile still
# has, and a soft 404 is indexed by crawlers as a real page.
probe "${MISS}/" 404 "text/html"

# An unknown TWIN is a 404 the reader can read. The bound is the point:
# before the handle_errors split this answered text/plain carrying a
# ~41 KB rendered HTML error page, which passes a status-and-type check
# and fails a human.
probe "${MISS}.md" 404 "text/plain" 2000

# ---------------------------------------------------------------------
# The cache split, checked EXHAUSTIVELY over the tree in the image
# rather than on a sampled URL.
# ---------------------------------------------------------------------
# This is the Caddyfile's most-argued decision and the one whose failure
# is least recoverable: a URL served `immutable` for a year cannot be
# un-pinned from the origin, so a theme upgrade that renames nothing
# reaches no reader who already holds the old bytes. The matchers were
# written against one build; this re-derives the two classes from
# whatever build is in the image and checks every member of both.
#
# The digest pattern is the Caddyfile's, deliberately including the
# `{8}` length anchor — a loose `[0-9a-f]+` also matches `lunr.da` and
# `lunr.de`, which is exactly the mistake being guarded against.
DIGEST_RE='\.[0-9a-f]{8}\.min\.(css|js)(\.map)?$'

pinned=0
unpinned=0
cache_failures_before=$failures
while IFS= read -r abs; do
	[ -n "$abs" ] || continue
	url="${abs#/usr/share/caddy}"
	cc=$(curl -s -o /dev/null -D- "${BASE}${url}" | tr -d '\r' |
		awk 'tolower($1)=="cache-control:"{$1="";sub(/^ /,"");print}')
	if printf '%s' "$abs" | grep -Eq "$DIGEST_RE"; then
		pinned=$((pinned + 1))
		case "$cc" in
		*immutable*) ;;
		*)
			printf 'FAILED  %-44s fingerprinted but not immutable (cc=%s)\n' "$url" "$cc" >&2
			failures=$((failures + 1))
			;;
		esac
	else
		unpinned=$((unpinned + 1))
		case "$cc" in
		*immutable*)
			printf 'FAILED  %-44s NOT fingerprinted yet pinned immutable (cc=%s)\n' "$url" "$cc" >&2
			failures=$((failures + 1))
			;;
		esac
	fi
done <<EOF
$(docker exec "$CID" sh -c "find /usr/share/caddy/assets -type f" | sort)
EOF

# Both classes must be non-empty, or "every member passed" is a claim
# about the empty set. A theme that stopped fingerprinting, or one that
# fingerprinted everything, silently turns half this check off.
if [ "$pinned" -lt 1 ] || [ "$unpinned" -lt 1 ]; then
	echo "FAILED: cache classes are not both populated (fingerprinted=$pinned, plain=$unpinned)." >&2
	echo "        One side empty means that side's assertion checked nothing." >&2
	failures=$((failures + 1))
elif [ "$failures" -eq "$cache_failures_before" ]; then
	printf 'ok      %-44s %s immutable / %s revalidating\n' "assets/ cache split" "$pinned" "$unpinned"
fi

if [ "$failures" -ne 0 ]; then
	echo "" >&2
	echo "FAILED: $failures probe(s) failed against $IMAGE" >&2
	exit 1
fi

echo ""
echo "docs-site smoke OK: 6 response probe(s) + $((pinned + unpinned)) asset(s) against $IMAGE"
