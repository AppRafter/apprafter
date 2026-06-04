# ADR 0041: Channel-tag version resolution — the operator reads the latest from a moving channel tag

## Status

Accepted (2026-06-04).

## Context

The `PlatformController` resolves the latest available platform-stack version per channel (`stable` / `beta` / `edge`) by listing the OCI tags of `ghcr.io/<owner>/platform-stack`, parsing each as semver, filtering by channel, and taking the maximum. This drives `status.availableVersion` and the `UpgradeAvailable` condition.

The OCI Distribution `tags/list` endpoint has no "newest-first" or "descending" mode — the spec returns tags lexically, and ghcr returns them in **push order (oldest first)**, so the newest version is on the **last** page. Finding the latest therefore requires reading **every** page (pagination via the `last` cursor), which is `O(pages)` and grows ~one entry per release, interleaved with cosign `.sig` tags. That pagination is also fragile against real-registry quirks: ghcr answers a past-the-end `last` query with `{"tags": null}` (not `[]`), which `oci-distribution` cannot deserialize — a bug that crashed the resolver every reconcile until the short-page/null-backstop fix landed.

The compatibility document (`compatibility.cue`/`.yaml`) shipped inside each chart is **cumulative** — the latest chart's copy lists every version with its change class and yank status. But it is fetched **from a tag**, and the *current* chart's copy cannot know about versions published *after* it, so it cannot by itself answer "what is the latest" without first enumerating tags (a chicken-and-egg).

The OCI registry is the operator's only guaranteed-reachable upstream (the chart lives there); reaching the GitHub Releases API (newest-first, but cross-stream-interleaved and a different egress host) from in-cluster is not assumed.

## Decision

We will resolve the channel-latest from a **moving channel tag** instead of a full tag listing:

- **Publish contract.** `platform-stack-publish.yml`, after pushing the immutable `platform-stack:<version>` chart, **moves the channel tag(s) the version belongs to** to that version's manifest, via `docker buildx imagetools create -t <repo>/platform-stack:<channel> <repo>/platform-stack:<version>` (Docker buildx is already on the runner; `landing-promote-to-prod.yml` uses the same mechanism). Channel membership follows the existing `stable ⊂ beta ⊂ edge` semantics:
  - a **stable** version (no pre-release) is the latest in **all three** → move `:stable`, `:beta`, `:edge`;
  - an **rc/beta** pre-release → move `:beta`, `:edge` (`:stable` stays at the prior stable);
  - any other pre-release (**edge**) → move `:edge` only.
  This assumes forward-only releases (each new version is the latest in its channels); a backport would need manual tag handling.

- **Resolver fast path.** The `PlatformController` fetches the compatibility document from `oci://<repo>/platform-stack:<channel>` in **one** poll cycle and takes the **latest non-yanked, channel-matching** version from its `compatibility` map — `O(1)`, no tag listing, no pagination.

- **Fallback.** When the channel tag is absent (a pre-contract chart, or a channel never published), the `:<channel>` fetch 404s and the resolver **falls back to the paginated tag listing** (`tags_in_channel`) + the prior `top_tag` + `resolve_non_yanked_latest`. The pagination path (with the short-page/null-backstop fix) is retained solely as this backstop.

In scope: the `stable`/`beta`/`edge` channel tags, the publish-workflow move step, the operator fast path + fallback. Out of scope: changing the immutable `:<version>` tags (the source of truth), the CLI bootstrap's initial-version resolution (it reads the GitHub Releases API; left as-is for now), and backport tag handling.

## Consequences

- **Easier:** resolution is `O(1)` (one compat fetch) and no longer grows with the release count or depends on registry pagination quirks; the `tags: null` failure mode is off the hot path; the compat doc is fetched once and already carries the yank/change metadata the controller needs.
- **Harder / neutral:** the publish workflow now owns a new responsibility (moving channel tags) — a release that fails to move a tag leaves the channel-latest stale (an under-report / missed upgrade, never a wrong deploy; self-heals on the next release); the channel tags are **mutable by design** (they are channel pointers, not provenance); two resolution paths (fast + fallback) exist until pre-contract charts age out.

## Alternatives considered

- **Full tag listing + pagination (the prior approach).** `O(pages)`, grows per release, and exposed the `tags: null` crash. Kept only as the fallback.
- **GitHub Releases API (newest-first).** The CLI bootstrap uses it; it returns releases newest-first so one page usually suffices — but platform-stack releases interleave with other tag streams (operator/landing/monorepo), it needs a different egress host (`api.github.com`) the in-cluster operator may not reach, and it has its own pagination. Rejected for the operator.
- **A single `:index` tag (newest overall) + in-resolver channel filtering.** Simpler publish (one tag), but the resolver must channel-filter a doc fetched from a possibly-prerelease chart and it leans harder on the cumulative-compat assumption. Per-channel tags are semantically cleaner (the resolver reads its own channel's pointer directly), so they were chosen.

## Risks

- **A publish run moves the version tag but not the channel tag** (workflow bug / partial failure) → the channel-latest is stale until the next release. Mitigation: the move is a required workflow step; the immutable `:<version>` tag remains the source of truth; a stale channel tag under-reports (no wrong deploy), and the fallback tag-listing still finds the real latest if the channel tag is missing entirely.
- **Mutable channel tags** could be confused with provenance. Accepted: they are explicitly channel pointers; `:<version>` is the immutable provenance, and `status` records the resolved concrete version.
- **Non-forward releases (backports)** would move a channel tag backward. Accepted as out of scope; handle manually if it ever arises.

## Owner

Platform / operator team.

## Re-evaluation

Revisit if multi-channel (`beta`/`edge`) usage grows enough to warrant first-class channel indices, if backport releases become routine, or if the fallback tag-listing can be removed once all live charts carry channel tags.

## References

- ADR 0028 (platform-stack OCI distribution), ADR 0026 (PlatformStack CRD).
- `operator/operator-controllers/platform-stack/src/{oci.rs,reconcile.rs}`; `.github/workflows/platform-stack-publish.yml`.
- The 2.4g manual walk, where the OCI-pagination resolver bug (`available=0.2.2`, then the `tags: null` crash) surfaced.
