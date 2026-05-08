// SPDX-License-Identifier: FSL-1.1-MIT
//! Library surface of the AppRafter admission webhook.

pub mod server;
pub mod validator;

pub use server::build_router;
pub use validator::{validate_application_spec, ValidationError};
