// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Argon2id for password slots (D.4a T22): the parameters a slot stores. The policy, the
//! derivation and the key schedule follow with the release floor.

/// Argon2id's cost parameters as a password slot stores them: memory in KiB, passes, lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Argon2idParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Argon2idParams {
    /// What a release build creates: 256 MiB, 3 passes, 4 lanes (T22).
    pub const RELEASE_DEFAULT: Self = Self {
        m_cost_kib: 262_144,
        t_cost: 3,
        p_cost: 4,
    };
}
