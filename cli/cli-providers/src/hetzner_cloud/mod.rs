// SPDX-License-Identifier: FSL-1.1-MIT
//! Built-in provider for the Hetzner Cloud API.

pub mod client;
pub mod provider;
pub mod server;
pub mod types;

pub use client::{HetznerCloudClient, DEFAULT_BASE_URL};
pub use provider::HetznerCloudProvider;
pub use server::{ServerSpec, APPRAFTER_LABEL, APPRAFTER_LABEL_VALUE};
pub use types::{
    ApiErrorDetails, ApiErrorEnvelope, Server, ServerCreateRequest, ServerCreateResponse,
    ServerListResponse, ServerStatus,
};
