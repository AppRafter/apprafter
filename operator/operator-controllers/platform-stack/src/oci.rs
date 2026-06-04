// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! OCI registry poll: list tags from an upstream chart repo,
//! filter to semver-valid + channel-matching, return the latest.
//!
//! Anonymous reads against ghcr.io's OCI Distribution Spec
//! endpoint. The `oci-distribution` crate handles the auth
//! handshake, but it does NOT auto-follow the registry's `Link`
//! pagination cursor — a single `list_tags` call returns only one
//! page. We therefore drive pagination ourselves via the OCI
//! `last` cursor (see `collect_all_tags`), accumulating every page
//! before sorting, so versions published on later pages stay
//! visible.

use std::future::Future;

use oci_distribution::client::ClientConfig;
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::{Client, Reference};
use semver::Version;
use thiserror::Error;

/// Page size requested per `list_tags` call (`n` query param).
/// Registries may cap or ignore this; the pagination loop tolerates
/// short pages and a registry that returns more than requested.
const PAGE_SIZE: usize = 100;

/// Hard ceiling on pages walked, bounding a misbehaving registry
/// that never signals exhaustion. At `PAGE_SIZE` tags per page this
/// is far above any realistic platform-stack tag count.
const MAX_PAGES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
    Edge,
}

impl Channel {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stable" => Some(Self::Stable),
            "beta" => Some(Self::Beta),
            "edge" => Some(Self::Edge),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum OciError {
    #[error("invalid reference {0:?}: {1}")]
    InvalidReference(String, String),
    #[error("registry IO: {0}")]
    Registry(String),
    #[error("no channel-matching versions found at {0:?}")]
    NoVersions(String),
}

/// Strip an `oci://` scheme prefix if present; the
/// `oci-distribution` `Reference` parser does not accept it.
fn strip_oci_scheme(url: &str) -> &str {
    url.strip_prefix("oci://").unwrap_or(url)
}

/// Decide whether a parsed semver belongs in the channel.
/// `stable` = no pre-release identifier. `beta` accepts
/// `-rc.*` and `-beta.*` (and stable). `edge` accepts everything
/// including arbitrary pre-release suffixes.
pub fn channel_matches(version: &Version, channel: Channel) -> bool {
    match channel {
        Channel::Stable => version.pre.is_empty(),
        Channel::Beta => {
            let pre = version.pre.as_str();
            pre.is_empty()
                || pre.starts_with("rc.")
                || pre.starts_with("beta.")
                || pre == "rc"
                || pre == "beta"
        }
        Channel::Edge => true,
    }
}

/// Fetch all tags from `upstream_url` and return every
/// channel-matching semver in descending order (newest first).
///
/// B.1.74a callers consume the full list to apply the yank
/// filter — they pull `compatibility.yaml` from the top tag,
/// then walk this Vec popping yanked entries off the front
/// until a non-yanked candidate is found.
///
/// Returns `Err(OciError::NoVersions)` when no tag in the
/// channel parses to a valid semver. Callers that just want
/// "newest in channel without any yank-awareness" use
/// `latest_in_channel`, which is a thin wrapper picking the
/// first entry.
pub async fn tags_in_channel(
    upstream_url: &str,
    channel: Channel,
) -> Result<Vec<Version>, OciError> {
    let bare = strip_oci_scheme(upstream_url);
    let reference: Reference = bare.parse().map_err(|e: oci_distribution::ParseError| {
        OciError::InvalidReference(bare.to_string(), e.to_string())
    })?;

    let client = Client::new(ClientConfig::default());
    let all_tags = collect_all_tags(PAGE_SIZE, |last| {
        let client = &client;
        let reference = &reference;
        async move {
            match client
                .list_tags(
                    reference,
                    &RegistryAuth::Anonymous,
                    Some(PAGE_SIZE),
                    last.as_deref(),
                )
                .await
            {
                Ok(r) => Ok(r.tags),
                // ghcr answers a past-the-end `last` query with
                // `{"tags": null}` (not `[]`); oci-distribution cannot
                // deserialize null into Vec<String>. Treat that single
                // error as exhaustion so the walk ends cleanly instead of
                // failing the whole reconcile.
                Err(e) if is_tag_exhaustion_error(&e.to_string()) => Ok(Vec::new()),
                Err(e) => Err(OciError::Registry(e.to_string())),
            }
        }
    })
    .await?;

    let versions = sort_tags_descending(all_tags, channel);
    if versions.is_empty() {
        return Err(OciError::NoVersions(upstream_url.to_string()));
    }
    Ok(versions)
}

/// ghcr answers a tag-list query whose `last` cursor is at/after the
/// final tag with `{"tags": null}` (not `{"tags": []}`); the
/// `oci-distribution` `TagResponse` deserialize then fails with
/// `invalid type: null, expected a sequence`. Recognise THAT specific
/// error so the pagination walk treats it as exhaustion instead of
/// failing the whole reconcile (the v0.2.12 resolver crash). Any other
/// registry error still propagates.
fn is_tag_exhaustion_error(msg: &str) -> bool {
    msg.contains("invalid type: null")
}

/// Walk OCI tag pages via the `last` cursor and accumulate every
/// tag (unsorted). `fetch(last)` returns one page given the cursor
/// of the previous page's final tag (`None` on the first call);
/// `page_size` is the `n` requested per call.
///
/// Termination is guarded four ways:
/// 1. a SHORT page (`len < page_size`) is the last one per the OCI
///    spec — stop WITHOUT fetching past it (a past-the-end fetch makes
///    ghcr return `{"tags": null}`, which the fetcher maps to an empty
///    page as a backstop for the exact-multiple case),
/// 2. an empty page ends the walk (registry exhaustion / the null
///    backstop),
/// 3. a NO-PROGRESS guard ends it when the cursor doesn't advance — a
///    registry that ignored `last` and re-served the same page (which
///    would otherwise loop forever), and
/// 4. a `MAX_PAGES` ceiling bounds any other misbehavior.
///
/// Extracted behind this fetcher seam so the pagination/termination
/// logic is unit-tested with canned pages and no network.
async fn collect_all_tags<F, Fut>(page_size: usize, mut fetch: F) -> Result<Vec<String>, OciError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<Vec<String>, OciError>>,
{
    let mut acc: Vec<String> = Vec::new();
    let mut last: Option<String> = None;

    for _ in 0..MAX_PAGES {
        let page = fetch(last.clone()).await?;
        if page.is_empty() {
            break;
        }
        let short = page.len() < page_size;
        let new_last = page.last().cloned();
        acc.extend(page);
        // A page shorter than `page_size` is the last page (OCI spec):
        // stop here so we never fetch past the end (ghcr answers a
        // past-the-end query with `{"tags": null}`).
        if short {
            break;
        }
        // Registry ignored `last` and re-served the same page:
        // the cursor didn't advance, so stop rather than spin.
        if new_last == last {
            break;
        }
        last = new_last;
    }

    Ok(acc)
}

/// Pure helper for `tags_in_channel`'s sort/filter logic.
/// Pulled out so unit tests cover the ordering invariant
/// without a network call.
fn sort_tags_descending(tags: Vec<String>, channel: Channel) -> Vec<Version> {
    let mut versions: Vec<Version> = tags
        .into_iter()
        .filter_map(|t| Version::parse(t.trim_start_matches('v')).ok())
        .filter(|v| channel_matches(v, channel))
        .collect();
    versions.sort();
    versions.reverse();
    versions
}

/// Fetch all tags from `upstream_url` and return the latest
/// semver in the requested channel. `upstream_url` examples:
/// `oci://ghcr.io/apprafter/platform-stack` or
/// `ghcr.io/apprafter/platform-stack`.
///
/// This function is yank-unaware — the B.1.74a reconcile loop
/// uses `tags_in_channel` + a compatibility-doc walk instead.
pub async fn latest_in_channel(upstream_url: &str, channel: Channel) -> Result<Version, OciError> {
    let mut versions = tags_in_channel(upstream_url, channel).await?;
    Ok(versions.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_stable_rejects_prerelease() {
        assert!(channel_matches(&"1.0.0".parse().unwrap(), Channel::Stable));
        assert!(!channel_matches(
            &"1.0.0-rc.1".parse().unwrap(),
            Channel::Stable
        ));
    }

    #[test]
    fn channel_beta_accepts_rc_and_beta_and_stable() {
        assert!(channel_matches(&"1.0.0".parse().unwrap(), Channel::Beta));
        assert!(channel_matches(
            &"1.0.0-rc.2".parse().unwrap(),
            Channel::Beta
        ));
        assert!(channel_matches(
            &"1.0.0-beta.5".parse().unwrap(),
            Channel::Beta
        ));
        assert!(!channel_matches(
            &"1.0.0-alpha.1".parse().unwrap(),
            Channel::Beta
        ));
    }

    #[test]
    fn channel_edge_accepts_everything() {
        assert!(channel_matches(&"1.0.0".parse().unwrap(), Channel::Edge));
        assert!(channel_matches(
            &"1.0.0-rc.2".parse().unwrap(),
            Channel::Edge
        ));
        assert!(channel_matches(
            &"1.0.0-alpha.1".parse().unwrap(),
            Channel::Edge
        ));
        assert!(channel_matches(
            &"1.0.0-experimental.7".parse().unwrap(),
            Channel::Edge
        ));
    }

    #[test]
    fn strip_oci_scheme_removes_prefix() {
        assert_eq!(strip_oci_scheme("oci://ghcr.io/x/y"), "ghcr.io/x/y");
        assert_eq!(strip_oci_scheme("ghcr.io/x/y"), "ghcr.io/x/y");
    }

    #[test]
    fn channel_parse_round_trips_known_values() {
        assert_eq!(Channel::parse("stable"), Some(Channel::Stable));
        assert_eq!(Channel::parse("beta"), Some(Channel::Beta));
        assert_eq!(Channel::parse("edge"), Some(Channel::Edge));
        assert_eq!(Channel::parse("nightly"), None);
    }

    #[test]
    fn sort_tags_descending_orders_newest_first_and_strips_v_prefix() {
        // B.1.74a contract: the yank-aware walk pops the front
        // of the returned Vec, so the ordering MUST be newest
        // first. Mixed `v`-prefixed and bare tags, plus a
        // non-semver tag, plus a tag the channel filter
        // rejects — all flow through one assertion.
        let tags = vec![
            "v0.1.20".to_string(),
            "0.1.19".to_string(),
            "v0.1.21-rc.1".to_string(), // stable channel rejects rc
            "v0.1.22".to_string(),
            "garbage".to_string(),
            "v0.1.18".to_string(),
        ];
        let sorted = sort_tags_descending(tags, Channel::Stable);
        let strs: Vec<String> = sorted.iter().map(|v| v.to_string()).collect();
        assert_eq!(
            strs,
            vec![
                "0.1.22".to_string(),
                "0.1.20".to_string(),
                "0.1.19".to_string(),
                "0.1.18".to_string(),
            ]
        );
    }

    #[test]
    fn sort_tags_descending_returns_empty_when_channel_rejects_all() {
        let tags = vec!["v0.1.0-rc.1".to_string(), "0.1.0-rc.2".to_string()];
        let sorted = sort_tags_descending(tags, Channel::Stable);
        assert!(sorted.is_empty());
    }

    use std::cell::RefCell;

    /// A canned-page fetcher: serves the supplied pages in order and
    /// records every `last` cursor it was called with. Implements the
    /// `FnMut(Option<String>) -> impl Future` shape `collect_all_tags`
    /// expects via its `fetch` method.
    struct PagedFetcher {
        pages: Vec<Vec<&'static str>>,
        idx: usize,
        calls: Vec<Option<String>>,
    }

    impl PagedFetcher {
        fn new(pages: Vec<Vec<&'static str>>) -> Self {
            Self {
                pages,
                idx: 0,
                calls: Vec::new(),
            }
        }

        fn fetch(
            &mut self,
            last: Option<String>,
        ) -> std::future::Ready<Result<Vec<String>, OciError>> {
            self.calls.push(last);
            let page = self
                .pages
                .get(self.idx)
                .map(|p| p.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            self.idx += 1;
            std::future::ready(Ok(page))
        }
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        // Pure, no-IO futures here; a minimal executor avoids pulling
        // a runtime into these unit tests.
        futures::executor::block_on(fut)
    }

    #[test]
    fn collect_all_tags_accumulates_full_pages_until_empty() {
        // Full pages (len == page_size) keep the walk going; an empty
        // page is the exhaustion signal. page_size=2.
        let fetcher = RefCell::new(PagedFetcher::new(vec![
            vec!["a", "b"],
            vec!["c", "d"],
            vec![],
        ]));
        let tags = block_on(collect_all_tags(2, |last| fetcher.borrow_mut().fetch(last))).unwrap();

        assert_eq!(tags, vec!["a", "b", "c", "d"]);
        // First call cursor None, then last tag of each prior page.
        assert_eq!(
            fetcher.borrow().calls,
            vec![None, Some("b".to_string()), Some("d".to_string())]
        );
    }

    #[test]
    fn collect_all_tags_stops_on_short_page_without_a_past_end_fetch() {
        // THE regression for the v0.2.12 resolver crash: ghcr answers a
        // query whose `last` is at/after the final tag with `{"tags":
        // null}`, which oci-distribution cannot deserialize, so the
        // reconcile errored every cycle and `availableVersion` froze. A
        // page SHORTER than page_size is the last page (OCI spec), so the
        // walk must STOP there and never issue that past-the-end fetch.
        // page_size=3: [a,b,c] is full, [d,e] is short → stop after
        // exactly two fetches (the third page must NOT be requested).
        let fetcher = RefCell::new(PagedFetcher::new(vec![
            vec!["a", "b", "c"],
            vec!["d", "e"],
            vec!["MUST-NOT-BE-FETCHED"],
        ]));
        let tags = block_on(collect_all_tags(3, |last| fetcher.borrow_mut().fetch(last))).unwrap();

        assert_eq!(tags, vec!["a", "b", "c", "d", "e"]);
        assert_eq!(
            fetcher.borrow().calls,
            vec![None, Some("c".to_string())],
            "must stop on the short last page, never fetch past the end"
        );
    }

    #[test]
    fn collect_all_tags_no_progress_guard_terminates() {
        // A registry that ignores `last` and always re-serves the same
        // FULL page (len == page_size, so the short-page guard does not
        // fire) must NOT infinite-loop. The first page advances the
        // cursor None→"b"; the second fetch returns the same page so the
        // cursor stays "b" — the no-progress guard breaks. Exactly two
        // fetches, well under MAX_PAGES.
        let same_page: Vec<&'static str> = vec!["a", "b"];
        let count = RefCell::new(0usize);
        let tags = block_on(collect_all_tags(2, |_last| {
            *count.borrow_mut() += 1;
            assert!(
                *count.borrow() <= MAX_PAGES,
                "no-progress guard did not fire; walk hit MAX_PAGES"
            );
            let page: Vec<String> = same_page.iter().map(|s| s.to_string()).collect();
            std::future::ready(Ok::<_, OciError>(page))
        }))
        .unwrap();
        assert_eq!(*count.borrow(), 2);
        assert_eq!(tags, vec!["a", "b", "a", "b"]);
    }

    #[test]
    fn collect_all_tags_respects_max_pages_ceiling() {
        // Cursor advances every page and each page is full (len 1 ==
        // page_size 1) so neither the short-page nor the no-progress
        // guard fires → bounded by MAX_PAGES.
        let counter = RefCell::new(0usize);
        let fetcher = |_last: Option<String>| {
            let mut c = counter.borrow_mut();
            *c += 1;
            // Unique tag per page → cursor always advances.
            let tag = format!("t{c}");
            std::future::ready(Ok::<_, OciError>(vec![tag]))
        };
        let tags = block_on(collect_all_tags(1, fetcher)).unwrap();
        assert_eq!(tags.len(), MAX_PAGES);
    }

    #[test]
    fn later_page_tag_is_included_in_final_sorted_result() {
        // Regression for the available=0.2.2 / invisible-0.2.11 bug:
        // the newest version arrives on a LATER page. With the old
        // single-page read it was dropped and the resolver reported a
        // stale first-page max. page_size=3: [0.2.0,0.2.1,0.2.2] is
        // full, [0.2.10,0.2.11] is the short last page; both accumulate.
        let fetcher = RefCell::new(PagedFetcher::new(vec![
            vec!["0.2.0", "0.2.1", "0.2.2"],
            vec!["0.2.10", "0.2.11"],
        ]));
        let all = block_on(collect_all_tags(3, |last| fetcher.borrow_mut().fetch(last))).unwrap();
        let sorted = sort_tags_descending(all, Channel::Stable);
        assert_eq!(
            sorted.first().map(|v| v.to_string()),
            Some("0.2.11".to_string())
        );
        assert!(
            sorted.iter().any(|v| v.to_string() == "0.2.11"),
            "0.2.11 from a later page must be present"
        );
    }

    #[test]
    fn is_tag_exhaustion_error_matches_ghcr_null_tags_only() {
        // ghcr's past-the-end `{"tags": null}` surfaces as this serde
        // message and MUST be treated as exhaustion (→ empty page); any
        // other registry error must propagate (not be swallowed).
        assert!(is_tag_exhaustion_error(
            "registry IO: invalid type: null, expected a sequence at line 1 column 46"
        ));
        assert!(!is_tag_exhaustion_error("registry IO: connection refused"));
        assert!(!is_tag_exhaustion_error("registry IO: 401 Unauthorized"));
    }
}
