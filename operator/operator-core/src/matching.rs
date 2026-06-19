// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure ResourceClaim → ServiceProvider matching. Zero kube/async deps
//! so the entire 2.3 algorithm is unit-testable without a cluster.

use std::collections::BTreeMap;

use kube::ResourceExt;

use crate::ServiceProvider;

/// A ServiceProvider candidate projected to the only fields matching
/// needs: name, service type, and labels.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub type_: String,
    pub labels: BTreeMap<String, String>,
}

impl Candidate {
    /// Project a ServiceProvider into a scheduler Candidate.
    pub fn from_provider(p: &ServiceProvider) -> Self {
        Candidate {
            name: p.name_any(),
            type_: p.spec.type_.clone(),
            labels: p.metadata.labels.clone().unwrap_or_default(),
        }
    }
}

/// A provider matches a claim iff the service types are equal AND the
/// provider's labels are a SUPERSET of the claim's selector (k8s
/// set-inclusion: every selector key present on the provider with the
/// same value; the provider may carry extra labels). An empty selector
/// matches any same-type provider.
pub fn matches(
    claim_type: &str,
    selector: &BTreeMap<String, String>,
    provider_type: &str,
    provider_labels: &BTreeMap<String, String>,
) -> bool {
    claim_type == provider_type
        && selector
            .iter()
            .all(|(k, v)| provider_labels.get(k) == Some(v))
}

/// Select the provider for a claim: filter qualifying candidates, then
/// pick the alphabetically-first by `name` (deterministic tie-break).
/// `None` when nothing qualifies.
pub fn select_provider(
    claim_type: &str,
    selector: &BTreeMap<String, String>,
    candidates: &[Candidate],
) -> Option<String> {
    candidates
        .iter()
        .filter(|c| matches(claim_type, selector, &c.type_, &c.labels))
        .map(|c| c.name.clone())
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lbls(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    fn cand(name: &str, type_: &str, labels: &[(&str, &str)]) -> Candidate {
        Candidate {
            name: name.into(),
            type_: type_.into(),
            labels: lbls(labels),
        }
    }

    #[test]
    fn type_mismatch_disqualifies_even_when_labels_match() {
        assert!(!matches(
            "pg",
            &lbls(&[("tier", "integrated")]),
            "redis",
            &lbls(&[("tier", "integrated")])
        ));
    }

    #[test]
    fn empty_selector_matches_any_same_type() {
        assert!(matches(
            "pg",
            &BTreeMap::new(),
            "pg",
            &lbls(&[("tier", "integrated")])
        ));
        assert!(matches("pg", &BTreeMap::new(), "pg", &BTreeMap::new()));
    }

    #[test]
    fn superset_matches_provider_with_extra_labels() {
        assert!(matches(
            "pg",
            &lbls(&[("tier", "integrated")]),
            "pg",
            &lbls(&[
                ("tier", "integrated"),
                ("location", "in-cluster"),
                ("compliance", "soc2")
            ]),
        ));
    }

    #[test]
    fn missing_selector_key_disqualifies() {
        assert!(!matches(
            "pg",
            &lbls(&[("tier", "integrated")]),
            "pg",
            &lbls(&[("location", "in-cluster")])
        ));
    }

    #[test]
    fn value_mismatch_disqualifies() {
        assert!(!matches(
            "pg",
            &lbls(&[("tier", "integrated")]),
            "pg",
            &lbls(&[("tier", "managed")])
        ));
    }

    #[test]
    fn select_picks_alphabetically_first_among_matches() {
        let cands = vec![
            cand("pg-zeta", "pg", &[("tier", "integrated")]),
            cand("pg-alpha", "pg", &[("tier", "integrated")]),
            cand("pg-mid", "pg", &[("tier", "integrated")]),
        ];
        assert_eq!(
            select_provider("pg", &lbls(&[("tier", "integrated")]), &cands).as_deref(),
            Some("pg-alpha")
        );
    }

    #[test]
    fn select_skips_type_and_label_mismatches() {
        let cands = vec![
            cand("redis-a", "redis", &[("tier", "integrated")]),
            cand("pg-managed", "pg", &[("tier", "managed")]),
            cand("pg-integrated", "pg", &[("tier", "integrated")]),
        ];
        assert_eq!(
            select_provider("pg", &lbls(&[("tier", "integrated")]), &cands).as_deref(),
            Some("pg-integrated")
        );
    }

    #[test]
    fn select_none_when_no_providers() {
        assert_eq!(
            select_provider("pg", &lbls(&[("tier", "integrated")]), &[]),
            None
        );
    }

    #[test]
    fn select_none_when_no_type_match() {
        let cands = vec![cand("redis-a", "redis", &[("tier", "integrated")])];
        assert_eq!(select_provider("pg", &BTreeMap::new(), &cands), None);
    }

    #[test]
    fn shared_disk_claim_does_not_match_owned_disk_provider() {
        let sel = BTreeMap::new();
        let mut labels = BTreeMap::new();
        labels.insert("tier".to_string(), "integrated".to_string());
        // a shared-disk claim must NOT bind an owned disk-local provider (type "disk")
        assert!(!matches("shared-disk", &sel, "disk", &labels));
        // ...but DOES match a shared-disk provider
        assert!(matches("shared-disk", &sel, "shared-disk", &labels));
    }
}
