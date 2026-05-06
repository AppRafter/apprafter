// SPDX-License-Identifier: FSL-1.1-MIT
use std::str::FromStr;

use cli_core::Tier;

#[test]
fn tier_from_str_parses_known_names() {
    assert_eq!(Tier::from_str("solo").unwrap(), Tier::Solo);
    assert_eq!(Tier::from_str("team").unwrap(), Tier::Team);
    assert_eq!(Tier::from_str("prod").unwrap(), Tier::Prod);
    assert_eq!(Tier::from_str("regulated").unwrap(), Tier::Regulated);
}

#[test]
fn tier_from_str_rejects_unknown() {
    assert!(Tier::from_str("enterprise").is_err());
}

#[test]
fn tier_display_round_trips() {
    for t in [Tier::Solo, Tier::Team, Tier::Prod, Tier::Regulated] {
        let s = t.to_string();
        assert_eq!(Tier::from_str(&s).unwrap(), t);
    }
}
