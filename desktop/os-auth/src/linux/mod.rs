// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Linux: polkit, which asks through the session's authentication agent, and PAM, which checks
//! the password from the app's own field where polkit cannot prompt.

mod authenticator;
pub mod pam;
pub mod polkit;

pub use authenticator::OsAuthenticator;
pub use pam::PasswordCheck;
