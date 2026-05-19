// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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

#[test]
fn from_str_rejects_unknown_tier_with_helpful_message() {
    use cli_core::CliError;
    use std::str::FromStr;
    let err = cli_core::Tier::from_str("staging").unwrap_err();
    match err {
        CliError::Other(msg) => {
            assert!(msg.contains("staging"), "{msg}");
            assert!(msg.contains("solo"), "{msg}");
        }
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn tier_level_assigns_monotonic_ordinals() {
    use cli_core::Tier;
    assert_eq!(Tier::Solo.level(), 1);
    assert_eq!(Tier::Team.level(), 2);
    assert_eq!(Tier::Prod.level(), 3);
    assert_eq!(Tier::Regulated.level(), 4);
}
