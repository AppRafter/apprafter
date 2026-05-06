// SPDX-License-Identifier: FSL-1.1-MIT
use cli_core::{CliError, Result};

#[test]
fn custom_error_renders_message() {
    let err = CliError::Other("boom".to_string());
    assert_eq!(err.to_string(), "boom");
}

#[test]
fn result_alias_compiles() {
    fn roundtrip() -> Result<u32> {
        Ok(42)
    }
    assert_eq!(roundtrip().unwrap(), 42);
}
