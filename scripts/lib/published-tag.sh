# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# published-tag.sh — the question every version guard asks first: is the
# version this tree declares ALREADY PUBLISHED? Sourced (not executed) by
# check-operator-version-bump.sh, check-cli-version-bump.sh,
# check-backup-runner-pin.sh, check-argocd-cue-cmp-drift.sh,
# check-version-coherence.sh and the chart drift step of
# .github/workflows/platform-stack-check.yml.
#
# ## Three answers, not two
#
# Each guard used to ask `git ls-remote --exit-code` and read ANY non-zero exit
# as "not published yet — the bump is in flight", then diff against the tag if
# it could fetch it and skip with a warning if it could not. Three different
# situations hid inside those two answers, and in each one the guard passed
# without comparing anything:
#
#   * the remote could not be asked at all — no network, no credentials, a
#     wrong remote URL. ls-remote exits 128 there, not 2, and the guard still
#     printed "OK: … in flight";
#   * the remote answered but lists NO tag of the series — a mirror or fork
#     pushed without tags. "Not published yet" and "cannot see any published
#     version" are then the same empty answer;
#   * the tag is listed but could not be fetched for the diff.
#
# Measured on a scratch clone carrying an operator change and no appVersion
# bump (WI-373): the first two printed "OK: operator/v… not yet on origin —
# appVersion bump is in flight" and exited 0, the third printed a warning and
# exited 0. A checkout that merely lacks tags is NOT one of these — the guard
# fetches the one tag it needs — which is why the answer below turns on what
# the REMOTE says, not on what the checkout happens to hold.
#
# So there are three answers. `in-flight` requires that the remote answered
# AND lists at least one other tag of the same series; anything the guard
# cannot decide is `undecided`.
#
# ## Undecided: fatal in CI, a warning on a laptop
#
# In CI (GITHUB_ACTIONS=true, or VERSION_GUARD_STRICT=1 anywhere) an undecided
# guard FAILS: a green check that compared nothing is exactly the silent pass
# these guards exist to prevent. Everywhere else it prints a ::warning:: that
# says the check did NOT run, and exits 0 — the guards also run as pre-commit
# hooks, and an offline commit must not be blocked by a check the pull request
# will run anyway. What changed locally is only the wording: it no longer says
# "OK".
#
# A brand-new tag series (no tag of it published yet) is undecided by this
# rule, deliberately; its first release has to be published before its guard
# can pass in CI. Every series in this repository has published tags.
#
# ## Usage
#
#     . "$(dirname "${BASH_SOURCE[0]}")/lib/published-tag.sh"
#     resolve_published_tag "$REMOTE" "operator/v" "$app_version"
#     case "$TAG_STATE" in
#         undecided) version_guard_undecided "$TAG_WHY" || exit 1; exit 0 ;;
#         in-flight) echo "OK: ${TAG} not yet on ${REMOTE} — bump in flight"; exit 0 ;;
#     esac
#     # published: refs/tags/$TAG is local and is the remote's tag; diff away.

# TAG, TAG_STATE and TAG_WHY are outputs, read by the sourcing script.
# shellcheck shell=bash disable=SC2034

# The line of a failed git command's output that says what went wrong: the
# first `fatal:`/`error:` line, else the last line. (The last line alone is
# often advice — "and the repository exists." — not the cause.)
_published_tag_cause() {
    local cause
    cause="$(grep -m1 -E '^(fatal|error):' <<<"$1" || true)"
    printf '%s' "${cause:-$(tail -1 <<<"$1")}"
}

# True when an undecided guard must fail rather than warn.
version_guard_strict() {
    [[ "${VERSION_GUARD_STRICT:-}" == 1 || "${GITHUB_ACTIONS:-}" == true ]]
}

# resolve_published_tag REMOTE PREFIX VERSION
#
# Sets TAG="${PREFIX}${VERSION}" and TAG_STATE to one of
#   published  — REMOTE lists TAG, and refs/tags/TAG is present locally and is
#                the same object (fetched here when it was missing);
#   in-flight  — REMOTE answered, lists other PREFIX* tags, and not this one;
#   undecided  — anything else; TAG_WHY says what.
# Always returns 0: what an undecided answer costs is the caller's decision
# (see version_guard_undecided).
resolve_published_tag() {
    local remote="$1" prefix="$2" version="$3"
    TAG="${prefix}${version}"
    TAG_STATE=undecided
    TAG_WHY=""

    local listing rc=0
    listing="$(git ls-remote --tags --refs "$remote" "refs/tags/${prefix}*" 2>&1)" || rc=$?
    if [[ $rc -ne 0 ]]; then
        TAG_WHY="could not list the ${prefix}* tags on ${remote} (git ls-remote exit ${rc}: $(_published_tag_cause "$listing")), so whether ${TAG} is published is unknown."
        return 0
    fi
    # Keep only ref lines; a transport may print notices on success.
    listing="$(grep -E $'^[0-9a-f]{40,64}\trefs/tags/' <<<"$listing" || true)"
    if [[ -z "$listing" ]]; then
        TAG_WHY="${remote} answered but lists no ${prefix}* tag at all, so \"${TAG} is not published yet\" cannot be told apart from \"no published version is visible\". The remote carries no tags (a mirror or fork pushed without them), or the series was renamed."
        return 0
    fi

    local remote_id
    remote_id="$(awk -v ref="refs/tags/${TAG}" '$2 == ref { print $1; exit }' <<<"$listing")"
    if [[ -z "$remote_id" ]]; then
        TAG_STATE=in-flight
        return 0
    fi

    local local_id
    local_id="$(git rev-parse --verify --quiet "refs/tags/${TAG}" 2>/dev/null || true)"
    if [[ -z "$local_id" ]]; then
        local fetch_err
        if ! fetch_err="$(git fetch --quiet --no-tags "$remote" "refs/tags/${TAG}:refs/tags/${TAG}" 2>&1)"; then
            TAG_WHY="${TAG} is published on ${remote} but could not be fetched for the diff: $(_published_tag_cause "$fetch_err")"
            return 0
        fi
        local_id="$(git rev-parse --verify --quiet "refs/tags/${TAG}" 2>/dev/null || true)"
    fi
    if [[ "$local_id" != "$remote_id" ]]; then
        TAG_WHY="the local ${TAG} (${local_id:-missing}) is not the tag ${remote} publishes (${remote_id}); a diff against it would compare the wrong tree. Refresh it: git fetch --force ${remote} tag ${TAG}"
        return 0
    fi
    TAG_STATE=published
}

# version_guard_undecided WHY
#
# Report that a guard could not run. Returns 1 when that is fatal (CI), 0 when
# it is only a warning (a laptop, a pre-commit hook).
version_guard_undecided() {
    if version_guard_strict; then
        printf '::error::%s\n' "$1" >&2
        printf 'This check did NOT run, and in CI that fails the job. The checkout must reach the remote and see its tags (actions/checkout with fetch-depth: 0).\n' >&2
        return 1
    fi
    printf '::warning::%s\n' "$1" >&2
    printf 'This check did NOT run. CI runs it and fails there if it still cannot (GITHUB_ACTIONS=true or VERSION_GUARD_STRICT=1).\n' >&2
    return 0
}
