// SPDX-License-Identifier: FSL-1.1-MIT
//! Library surface of the AppRafter operator binary.
//!
//! Exposes the axum router builder so integration tests can drive
//! it via `tower::ServiceExt::oneshot`.

pub mod server;

pub use server::build_router;
