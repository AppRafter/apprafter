// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use cli_core::logging;

#[test]
fn init_logging_is_idempotent() {
    // Calling twice must not panic; the second call is a no-op.
    logging::init();
    logging::init();
}
