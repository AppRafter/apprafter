#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-ecr-seed.sh — every ECR Public image Argo CD's chart renders is one
# the e2e walks can seed from another registry, and one users can pull.
#
# ## Why
#
# ECR Public serves anonymous pulls 500 GB a month per source IP, and on
# GitHub's shared runner IPs that runs out in the last days of a month: one
# walk failed on 2026-08-31 and three on 2026-09-30, all on argocd-redis
# answering 429 "toomanyrequests: Data limit exceeded". So before bootstrap
# the walks put those images into the kind node from mirror.gcr.io or Docker
# Hub (seed_ecr_public_images in e2e/lib.sh, which explains the mechanism).
#
# That seed is best effort on purpose: when it cannot seed an image it warns
# and the node pulls from ECR as before, so a walk never fails harder than it
# did without it. The cost is that a regression is quiet: an argo-cd chart
# bump or a values change that defeats it would bring the month-end failures
# back with nothing but a WARN in a green walk. This check makes it loud when
# the chart or values change, not on the 30th. It fails when:
#
#   - the chart cannot be rendered with either value set that installs it
#     (the loader's, and the platform-stack component's), or the render
#     yields no image at all, which means the extraction itself broke;
#   - an ECR Public image is digest-pinned or outside docker/library, so the
#     seed cannot place it;
#   - neither seed source serves it;
#   - ECR Public itself answers 404 for it. The seed would let every walk pass
#     on the mirror's copy of an image users cannot pull, which an unseeded
#     walk would have caught. Any other ECR answer (429 at month end, a
#     timeout) is reported and does not fail, so this check cannot flake on
#     the very quota it works around;
#   - the image's container sets imagePullPolicy Always, or a :latest or
#     missing tag that defaults to it: the kubelet would then ask ECR on
#     every start and the seeded copy would change nothing.
#
# It runs the walks' own functions, so it checks what they will do. The node
# side of the seed (crictl and ctr inside a kind node) is not exercised here:
# that shows as "seeded … from …" lines, or a ::warning:: annotation, in the
# walks themselves.
#
# Needs helm, yq, jq, the pinned cue and the network: the chart repository,
# and one anonymous HEAD per source and per image. A HEAD is not a pull, so
# it spends no data quota.
#
# Usage: bash scripts/check-ecr-seed.sh
# Exit 0 = every ECR Public image the chart renders is seedable, served by a
# seed source and by ECR Public, and pulled IfNotPresent.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
# shellcheck source=e2e/lib.sh
source "$REPO_ROOT/e2e/lib.sh"

command -v helm >/dev/null 2>&1 || {
    echo "::error::helm is not on PATH — run under \`nix develop\`" >&2
    exit 2
}
if command -v yq >/dev/null 2>&1; then
    YQ=(yq)
else
    YQ=(nix run nixpkgs#yq-go --)
fi

MANIFEST_ACCEPT='application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.list.v2+json,application/vnd.oci.image.manifest.v1+json,application/vnd.docker.distribution.manifest.v2+json'
CURL=(curl -sS --max-time 30 --retry 3 --retry-delay 2)

# manifest_head <ref>
#   Ask a registry anonymously, with HEAD, for <host>/<repository>:<tag>,
#   following the bearer-token challenge Docker Hub and ECR Public send. Sets
#   HEAD_STATUS to the final HTTP status and HEAD_DIGEST to the digest served
#   (empty unless 200). Returns 1 when no HTTP answer came back at all.
manifest_head() {
    local ref="$1" host rest repo tag url headers challenge realm service scope token
    HEAD_STATUS=""
    HEAD_DIGEST=""
    host="${ref%%/*}"
    rest="${ref#*/}"
    repo="${rest%:*}"
    tag="${rest##*:}"
    [ "$host" = "docker.io" ] && host="registry-1.docker.io"
    url="https://${host}/v2/${repo}/manifests/${tag}"

    headers="$("${CURL[@]}" -I -H "Accept: ${MANIFEST_ACCEPT}" "$url")" || return 1
    HEAD_STATUS="$(printf '%s\n' "$headers" | awk 'NR == 1 { print $2 }')"
    if [ "$HEAD_STATUS" = "401" ]; then
        challenge="$(printf '%s\n' "$headers" | tr -d '\r' | awk 'tolower($1) == "www-authenticate:" { $1 = ""; print; exit }')"
        realm="$(printf '%s' "$challenge" | sed -nE 's/.*realm="([^"]+)".*/\1/p')"
        service="$(printf '%s' "$challenge" | sed -nE 's/.*service="([^"]+)".*/\1/p')"
        scope="$(printf '%s' "$challenge" | sed -nE 's/.*scope="([^"]+)".*/\1/p')"
        [ -n "$realm" ] || return 1
        token="$("${CURL[@]}" -f -G "$realm" \
            ${service:+--data-urlencode "service=${service}"} \
            ${scope:+--data-urlencode "scope=${scope}"} \
            | jq -r '.token // .access_token // empty')" || return 1
        [ -n "$token" ] || return 1
        headers="$("${CURL[@]}" -I -H "Accept: ${MANIFEST_ACCEPT}" \
            -H "Authorization: Bearer ${token}" "$url")" || return 1
        HEAD_STATUS="$(printf '%s\n' "$headers" | awk 'NR == 1 { print $2 }')"
    fi
    if [ "$HEAD_STATUS" = "200" ]; then
        HEAD_DIGEST="$(printf '%s\n' "$headers" | tr -d '\r' \
            | awk 'tolower($1) == "docker-content-digest:" { print $2; exit }')"
    fi
}

if ! manifests="$(argocd_charts_rendered)"; then
    echo "::error::could not render Argo CD's chart with the loader's or the platform-stack component's values (see the output above), so the walks could not list the ECR Public images to seed" >&2
    exit 1
fi
if ! images="$(printf '%s\n' "$manifests" | ecr_public_images_in)"; then
    echo "::error::rendered Argo CD's chart but read no image at all from it; the chart always renders its own, so the extraction in ecr_public_images_in (e2e/lib.sh) is broken and the walks would seed nothing" >&2
    exit 1
fi
if [ -z "$images" ]; then
    echo "OK: Argo CD's chart renders no ECR Public image; nothing for the walks to seed"
    exit 0
fi

failed=0
for image in $images; do
    # The pull policy the whole mechanism depends on, for every container in
    # either render that runs this image.
    policies="$(printf '%s\n' "$manifests" | IMAGE="$image" "${YQ[@]}" -N \
        '.. | select(tag == "!!map" and has("image") and has("name")) | select(.image == strenv(IMAGE)) | (.imagePullPolicy // "")')"
    tail_ref="${image##*/}"
    while IFS= read -r policy; do
        case "$policy" in
            IfNotPresent | Never) ;;
            "")
                # Unset: the kubelet defaults to Always for a :latest or
                # missing tag, and to IfNotPresent otherwise or for a digest.
                defaults_to_always=""
                case "$tail_ref" in
                    *@*) ;;
                    *:*) [ "${tail_ref##*:}" != "latest" ] || defaults_to_always=1 ;;
                    *) defaults_to_always=1 ;;
                esac
                if [ -n "$defaults_to_always" ]; then
                    echo "::error::a container runs ${image} with no imagePullPolicy and a :latest or missing tag, so the kubelet pulls it from ECR Public on every start and the seed changes nothing. Set redis.image.imagePullPolicy (or global.image.imagePullPolicy) to IfNotPresent." >&2
                    failed=1
                fi
                ;;
            *)
                echo "::error::a container runs ${image} with imagePullPolicy ${policy}, so the kubelet asks ECR Public on every start and the seed changes nothing. Keep it IfNotPresent." >&2
                failed=1
                ;;
        esac
    done <<<"$policies"

    if ! sources="$(ecr_public_seed_sources "$image")"; then
        case "$image" in
            *@*) echo "::error::${image} is digest-pinned; seed_ecr_public_images in e2e/lib.sh does not seed digest-pinned references yet, so every nightly pulls it from ECR Public's anonymous quota. Pin by tag here, or teach the seed to tag the digest form CRI stores." >&2 ;;
            *) echo "::error::${image} is an ECR Public image outside docker/library, so the walks cannot seed it from another registry and every nightly pulls it from ECR Public's anonymous quota. Override the image in platform-stack/cue/loader_values.cue, or extend ecr_public_seed_sources in e2e/lib.sh with a source that serves it." >&2 ;;
        esac
        failed=1
        continue
    fi

    seed_source=""
    seed_digest=""
    for source in $sources; do
        if manifest_head "$source" && [ "$HEAD_STATUS" = "200" ] && [ -n "$HEAD_DIGEST" ]; then
            seed_source="$source"
            seed_digest="$HEAD_DIGEST"
            break
        fi
        echo "  ${source} does not serve it (HTTP ${HEAD_STATUS:-no answer}); trying the next source"
    done
    if [ -z "$seed_source" ]; then
        echo "::error::no seed source serves ${image} (tried: $(printf '%s' "$sources" | tr '\n' ' ')), so the walks fall back to ECR Public's anonymous quota for it" >&2
        failed=1
        continue
    fi

    # ECR Public itself: the image users pull. Only a definite "not here"
    # fails; month-end 429s and timeouts must not make this check flaky.
    if manifest_head "$image"; then
        case "$HEAD_STATUS" in
            200)
                if [ "$HEAD_DIGEST" = "$seed_digest" ]; then
                    echo "OK: ${image} is seeded from ${seed_source}, the same content ECR Public serves (${seed_digest})"
                else
                    echo "OK: ${image} is seeded from ${seed_source} (${seed_digest})"
                    echo "::warning::ECR Public serves ${image} as ${HEAD_DIGEST} but ${seed_source} as ${seed_digest}: the walks run a different build of the same tag than users pull. Usually a rebuilt tag that one registry has not caught up with."
                fi
                ;;
            404)
                echo "::error::ECR Public does not serve ${image} (HTTP 404), while ${seed_source} does: the walks would pass on the seeded copy and every user's bootstrap would fail to pull it" >&2
                failed=1
                ;;
            *)
                echo "OK: ${image} is seeded from ${seed_source} (${seed_digest})"
                echo "::warning::ECR Public answered HTTP ${HEAD_STATUS} for ${image}, so whether users can pull it was not checked (429 is the month-end quota this works around)"
                ;;
        esac
    else
        echo "OK: ${image} is seeded from ${seed_source} (${seed_digest})"
        echo "::warning::ECR Public could not be reached for ${image}, so whether users can pull it was not checked"
    fi
done

exit "$failed"
