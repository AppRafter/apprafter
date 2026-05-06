// SPDX-License-Identifier: FSL-1.1-MIT
use cli_core::Tier;
use cli_state::{State, StatePaths};

#[test]
fn load_or_default_returns_default_on_fresh_dir() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());

    let state = State::load_or_default(&paths).unwrap();
    assert_eq!(state, State::default());
    assert_eq!(state.tier, None);
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());

    let original = State {
        cluster_name: Some("solo-1".to_string()),
        tier: Some(Tier::Solo),
        ..Default::default()
    };
    original.save(&paths).unwrap();

    let reloaded = State::load_or_default(&paths).unwrap();
    assert_eq!(reloaded, original);
}

#[test]
fn load_rejects_corrupt_state_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());
    std::fs::create_dir_all(paths.state_file().parent().unwrap()).unwrap();
    std::fs::write(paths.state_file(), b"{not json}").unwrap();

    let err = State::load_or_default(&paths).unwrap_err();
    assert!(
        matches!(err, cli_core::CliError::InvalidState { .. }),
        "got: {err}"
    );
}
