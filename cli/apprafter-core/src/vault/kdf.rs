// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Argon2id for password slots and the key schedule of every slot (D.4a T18, T22, T23; overview
//! §3.7).
//!
//! **Policy.** One [`KdfPolicy`] value decides which Argon2id parameters a build creates and
//! accepts. Every check takes the policy as a value, so the release rule is tested in every
//! build ([`KdfPolicy::RELEASE`]) whichever one [`KdfPolicy::active`] returns. The fast
//! parameters exist only in tests and behind the `insecure-fast-kdf` feature; four guards keep
//! them out of a release: L1 (the release floor table, here), L2 (`active()` matches the
//! feature, here), L3 (the release binary is checked in `release-cli.yml`, d4a-e) and L4 (no
//! manifest outside `[dev-dependencies]` turns the feature on, `tests/guards.rs`).
//!
//! **Memory.** argon2 0.6.0 frees its own working memory without zeroing it (its `Blocks` drop
//! only deallocates, `argon2-0.6.0/src/block.rs:190-200`), so the derivation brings its own,
//! a `Zeroizing<Vec<argon2::Block>>` that is zeroed before it is freed.

use zeroize::Zeroizing;

use super::error::{VaultError, VaultResult};
use super::format::SALT_LEN;
use crate::context::SecretString;

/// The 32 bytes a slot secret yields before HKDF: the Argon2id output, or a recovery key.
pub(crate) const MATERIAL_LEN: usize = 32;

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

/// Which Argon2id parameters a build creates and accepts. The fields are private: a policy is
/// one of the constants below, and no caller can make a weaker one.
#[derive(Debug, PartialEq, Eq)]
pub struct KdfPolicy {
    create: Argon2idParams,
    floor_m_cost_kib: u32,
    floor_m_times_t: u64,
    ceiling: Argon2idParams,
}

/// Every policy's ceiling, checked before any allocation: RFC 9106's first option (2 GiB),
/// 16 passes, 16 lanes.
const CEILING: Argon2idParams = Argon2idParams {
    m_cost_kib: 2_097_152,
    t_cost: 16,
    p_cost: 16,
};

#[cfg(feature = "insecure-fast-kdf")]
const ACTIVE: &KdfPolicy = &KdfPolicy::INSECURE_FAST;
#[cfg(not(feature = "insecure-fast-kdf"))]
const ACTIVE: &KdfPolicy = &KdfPolicy::RELEASE;

impl KdfPolicy {
    /// Release builds: create [`Argon2idParams::RELEASE_DEFAULT`]; refuse m < 64 MiB or
    /// m·t < 192 MiB·passes (RFC 9106's second option as a cost product) on create and on open.
    pub const RELEASE: KdfPolicy = KdfPolicy {
        create: Argon2idParams::RELEASE_DEFAULT,
        floor_m_cost_kib: 65_536,
        floor_m_times_t: 196_608,
        ceiling: CEILING,
    };

    /// Tests and walks only: microsecond derivations a release build refuses. It accepts
    /// release parameters too, so a test build still opens a real vault.
    #[cfg(any(test, feature = "insecure-fast-kdf"))]
    pub const INSECURE_FAST: KdfPolicy = KdfPolicy {
        create: Argon2idParams {
            m_cost_kib: 32,
            t_cost: 1,
            p_cost: 1,
        },
        floor_m_cost_kib: 8,
        floor_m_times_t: 8,
        ceiling: CEILING,
    };

    /// This build's policy: [`KdfPolicy::INSECURE_FAST`] under the `insecure-fast-kdf`
    /// feature, else [`KdfPolicy::RELEASE`].
    pub fn active() -> &'static KdfPolicy {
        ACTIVE
    }

    pub fn is_release(&self) -> bool {
        *self == KdfPolicy::RELEASE
    }

    /// The parameters a new password slot gets under this policy.
    pub fn create_params(&self) -> Argon2idParams {
        self.create
    }

    /// `KdfOutOfRange` above the ceiling or for parameters Argon2id cannot run (t or p of 0,
    /// m below 8·p) — checked first, before any allocation; `KdfTooWeak` below the floor.
    pub fn check(&self, params: &Argon2idParams) -> VaultResult<()> {
        let ceiling = &self.ceiling;
        let runnable = params.t_cost >= 1
            && params.p_cost >= 1
            && u64::from(params.m_cost_kib) >= 8 * u64::from(params.p_cost);
        if !runnable
            || params.m_cost_kib > ceiling.m_cost_kib
            || params.t_cost > ceiling.t_cost
            || params.p_cost > ceiling.p_cost
        {
            return Err(VaultError::KdfOutOfRange { params: *params });
        }
        if params.m_cost_kib < self.floor_m_cost_kib
            || u64::from(params.m_cost_kib) * u64::from(params.t_cost) < self.floor_m_times_t
        {
            return Err(VaultError::KdfTooWeak {
                params: *params,
                floor_m_cost_kib: self.floor_m_cost_kib,
                floor_m_times_t: self.floor_m_times_t,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    /// Argon2id derivations this thread ran: tests assert "nothing was derived".
    pub(crate) static DERIVATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Two tests derive at release strength, one at a time (D.4a T23, §7): the floor cross-check
/// once (64 MiB) and `password_recovery_vector` three times (256 MiB); the ignored timing test
/// takes it too.
#[cfg(test)]
pub(crate) static HEAVY_KDF: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `Argon2id(v0x13, password as typed, salt, params, 32 bytes)` (§3.7), after `policy.check`
/// and into a working memory of ours that is zeroed before it is freed.
pub(crate) fn derive_password_material(
    password: &SecretString,
    salt: &[u8; SALT_LEN],
    params: &Argon2idParams,
    policy: &KdfPolicy,
) -> VaultResult<Zeroizing<[u8; MATERIAL_LEN]>> {
    policy.check(params)?;
    let out_of_range = |_: argon2::Error| VaultError::KdfOutOfRange { params: *params };
    let argon_params = argon2::Params::new(
        params.m_cost_kib,
        params.t_cost,
        params.p_cost,
        Some(MATERIAL_LEN),
    )
    .map_err(out_of_range)?;
    let argon = argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon_params,
    );
    let blocks = argon.params().block_count();
    let mut memory: Zeroizing<Vec<argon2::Block>> = Zeroizing::new(Vec::new());
    memory
        .try_reserve_exact(blocks)
        .map_err(|_| VaultError::KdfOutOfRange { params: *params })?;
    memory.resize(blocks, argon2::Block::new());
    let mut material = Zeroizing::new([0u8; MATERIAL_LEN]);
    argon
        .hash_password_into_with_memory(
            password.expose().as_bytes(),
            salt,
            &mut material[..],
            &mut memory[..],
        )
        .map_err(out_of_range)?;
    #[cfg(test)]
    DERIVATIONS.with(|n| n.set(n.get() + 1));
    Ok(material)
}

/// A slot's two keys: `kek` wraps the data key; `key_check` tells a wrong secret
/// (`WrongSecret`) from a damaged slot (`SlotCorrupt`) and commits the slot to one key. Both
/// are zeroed on drop.
pub(crate) struct SlotKeys {
    pub(crate) kek: Zeroizing<[u8; 32]>,
    pub(crate) key_check: Zeroizing<[u8; 32]>,
}

/// `HKDF-SHA256(salt = slot salt, ikm = material)`, expanded twice with the slot kind in the
/// info (§3.7). `material` is a slot's bytes before HKDF: 32 for a lock password or a recovery
/// key, 64 for an account password (Argon2id's output, then the machine secret: D.4b B54).
/// HKDF takes any length, so the frozen 32-byte vectors keep their keys (B3). The `Hkdf`
/// value — its HMAC state is keyed with the PRK, which hkdf 0.12 cannot wipe (T29) — lives
/// only until this function returns.
pub(crate) fn slot_keys(material: &[u8], salt: &[u8; SALT_LEN], kind: u8) -> SlotKeys {
    const INFO_KEK: &[u8] = b"apprafter-vault/v1/kek/";
    const INFO_CHECK: &[u8] = b"apprafter-vault/v1/key-check/";
    let kind = [kind];
    let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(Some(&salt[..]), material);
    let mut kek = Zeroizing::new([0u8; 32]);
    let mut key_check = Zeroizing::new([0u8; 32]);
    hkdf.expand_multi_info(&[INFO_KEK, &kind[..]], &mut kek[..])
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    hkdf.expand_multi_info(&[INFO_CHECK, &kind[..]], &mut key_check[..])
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    SlotKeys { kek, key_check }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_alloc;
    use crate::vault::testutil::{hex, seq_array};
    use std::time::{Duration, Instant};

    const PASSWORD: &str = "correct horse battery staple";
    const SALT: &[u8; 16] = b"apprafter-salt16";

    fn params(m_cost_kib: u32, t_cost: u32, p_cost: u32) -> Argon2idParams {
        Argon2idParams {
            m_cost_kib,
            t_cost,
            p_cost,
        }
    }

    #[derive(Debug, PartialEq)]
    enum Verdict {
        Ok,
        TooWeak,
        OutOfRange,
    }

    fn verdict(policy: &KdfPolicy, p: Argon2idParams) -> Verdict {
        match policy.check(&p) {
            Ok(()) => Verdict::Ok,
            Err(VaultError::KdfTooWeak { .. }) => Verdict::TooWeak,
            Err(VaultError::KdfOutOfRange { .. }) => Verdict::OutOfRange,
            Err(other) => panic!("{p:?}: {other:?}"),
        }
    }

    /// L1 (T22): the release floor, value-tested in every build whatever the active policy.
    #[test]
    fn release_policy_refuses_below_floor() {
        let rows = [
            (params(262_144, 3, 4), Verdict::Ok),      // RELEASE_DEFAULT
            (params(65_536, 3, 4), Verdict::Ok),       // RFC 9106 option 2, exactly the floor
            (params(98_304, 2, 4), Verdict::Ok),       // m·t exactly 196 608
            (params(2_097_152, 1, 4), Verdict::Ok),    // RFC 9106 option 1
            (params(2_097_152, 16, 16), Verdict::Ok),  // the ceiling itself
            (params(65_535, 3, 4), Verdict::TooWeak),  // m one below the floor
            (params(65_536, 2, 4), Verdict::TooWeak),  // m on the floor, m·t below
            (params(98_303, 2, 4), Verdict::TooWeak),  // m·t one below
            (params(131_072, 1, 4), Verdict::TooWeak), // m above, one pass
            (params(19_456, 2, 1), Verdict::TooWeak),  // OWASP's online-server minimum
            (params(32, 1, 1), Verdict::TooWeak),      // INSECURE_FAST's
            (params(2_097_153, 1, 4), Verdict::OutOfRange),
            (params(262_144, 17, 4), Verdict::OutOfRange),
            (params(262_144, 3, 17), Verdict::OutOfRange),
            (params(u32::MAX, 1, 1), Verdict::OutOfRange),
            (params(262_144, 0, 4), Verdict::OutOfRange),
            (params(262_144, 3, 0), Verdict::OutOfRange),
            (params(31, 1, 4), Verdict::OutOfRange), // below 8·p: Argon2id cannot run
            (params(4, 1, 1), Verdict::OutOfRange),  // too weak too: the ceiling check comes first
        ];
        for (p, expected) in rows {
            assert_eq!(verdict(&KdfPolicy::RELEASE, p), expected, "{p:?}");
        }
    }

    #[test]
    fn fast_parameters_fail_release_and_the_fast_policy_opens_release_vaults() {
        let fast = KdfPolicy::INSECURE_FAST.create_params();
        assert_eq!(verdict(&KdfPolicy::RELEASE, fast), Verdict::TooWeak);
        assert_eq!(verdict(&KdfPolicy::INSECURE_FAST, fast), Verdict::Ok);
        assert_eq!(
            verdict(&KdfPolicy::INSECURE_FAST, Argon2idParams::RELEASE_DEFAULT),
            Verdict::Ok
        );
        assert_eq!(
            KdfPolicy::RELEASE.create_params(),
            Argon2idParams::RELEASE_DEFAULT
        );
        assert!(KdfPolicy::RELEASE.is_release());
        assert!(!KdfPolicy::INSECURE_FAST.is_release());
    }

    /// L2 (T23): the active policy is the release one exactly when the feature is off.
    /// Unconditional, so it runs (and means something) in both `cargo test` and
    /// `cargo test --all-features`.
    #[test]
    fn active_policy_matches_the_feature() {
        assert_eq!(
            KdfPolicy::active().is_release(),
            !cfg!(feature = "insecure-fast-kdf")
        );
    }

    #[test]
    fn the_ceiling_is_refused_before_any_allocation() {
        let password = SecretString::new(PASSWORD);
        let huge = params(u32::MAX, 1, 1);
        let derived_before = DERIVATIONS.with(std::cell::Cell::get);
        let mut fastest = Duration::MAX;
        for _ in 0..5 {
            test_alloc::reset_largest();
            let start = Instant::now();
            let refused = derive_password_material(&password, SALT, &huge, &KdfPolicy::RELEASE);
            fastest = fastest.min(start.elapsed());
            assert!(matches!(refused, Err(VaultError::KdfOutOfRange { .. })));
            assert!(
                test_alloc::largest() < 1024 * 1024,
                "{} bytes were asked for before the refusal",
                test_alloc::largest()
            );
        }
        assert!(fastest < Duration::from_millis(10), "took {fastest:?}");
        assert_eq!(DERIVATIONS.with(std::cell::Cell::get), derived_before);
    }

    /// libargon2 20190702's answers (`nix shell nixpkgs#libargon2`, D.4a crypto brief §12.1),
    /// reproduced through our call path: Argon2id v0x13, no secret, no associated data.
    #[test]
    fn libargon2_cross_check_fast() {
        let out = derive_password_material(
            &SecretString::new(PASSWORD),
            SALT,
            &params(32, 1, 1),
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap();
        assert_eq!(
            hex(&out[..]),
            "83687d38799b4605d917a405b971e613853511e407019cbb519207d44759255b"
        );
    }

    /// libargon2 at the release floor (64 MiB, t = 3, p = 4), which no vector covers: one of
    /// the two tests that derive at release strength (T23), behind HEAVY_KDF. The 256 MiB
    /// default is checked against the generator's libargon2 output by
    /// `password_recovery_vector`, and against libargon2's own by `release_params_timing`.
    #[test]
    fn libargon2_cross_check_floor() {
        let _heavy = HEAVY_KDF.lock().unwrap_or_else(|e| e.into_inner());
        let out = derive_password_material(
            &SecretString::new(PASSWORD),
            SALT,
            &params(65_536, 3, 4),
            &KdfPolicy::RELEASE,
        )
        .unwrap();
        assert_eq!(
            hex(&out[..]),
            "3dabb2a978a05c8297521799a93f6aecab376f50fae0c483cd7b81ee47c28a04"
        );
    }

    /// argon2 0.6.0 frees its own working memory unzeroed; ours is zeroed before it is freed.
    #[test]
    fn the_working_memory_is_zeroed_before_it_is_freed() {
        let before = test_alloc::unzeroed_block_frees();
        let out = derive_password_material(
            &SecretString::new(PASSWORD),
            SALT,
            &params(64, 1, 1),
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap();
        assert_eq!(out.len(), MATERIAL_LEN);
        assert_eq!(
            test_alloc::unzeroed_block_frees(),
            before,
            "an Argon2 working memory was freed holding data"
        );
    }

    /// The detector above can fail: argon2's own allocating call frees its memory unzeroed.
    #[test]
    fn the_detector_sees_argon2s_own_unzeroed_memory() {
        let before = test_alloc::unzeroed_block_frees();
        let argon = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(64, 1, 1, Some(32)).unwrap(),
        );
        let mut out = [0u8; 32];
        argon
            .hash_password_into(PASSWORD.as_bytes(), SALT, &mut out)
            .unwrap();
        assert_eq!(test_alloc::unzeroed_block_frees(), before + 1);
    }

    /// The key schedule against the independent generator's fast-params vector
    /// (`tests/vault_vectors/v1/fast-params/inputs.json`): one slot of each kind.
    #[test]
    fn slot_keys_follow_the_schedule() {
        let material: [u8; 32] = crate::vault::testutil::unhex(
            "f049865a2099fc22fe213398e42bc6d1cd3796f2567b556835bb01f8ecbd37dd",
        )
        .try_into()
        .unwrap();
        let keys = slot_keys(&material, &seq_array(0x80), 0x01);
        assert_eq!(
            hex(&keys.kek[..]),
            "69c3f1c15f063a496e85b6d07e433ceda2e5634c590764c283b34f53400326e0"
        );
        assert_eq!(
            hex(&keys.key_check[..]),
            "da4ce96dc431ff7760669d76972a74b70060fa5f0c7ddd39ed39a46e28e9d8be"
        );
        let keys = slot_keys(&seq_array::<32>(0x00), &seq_array(0xb8), 0x02);
        assert_eq!(
            hex(&keys.kek[..]),
            "9564e57beecdb2fbefe4f9e3fb568594732497e82295d0b4c58dfba34fe1c160"
        );
        assert_eq!(
            hex(&keys.key_check[..]),
            "af94e6ba00de085ec7093f25c8089817fded1dec500590a2e35311015c27d99b"
        );
        // The kind is part of the info: the same material keys another kind differently.
        let other_kind = slot_keys(&seq_array::<32>(0x00), &seq_array(0xb8), 0x01);
        assert_ne!(other_kind.kek[..], keys.kek[..]);
    }

    /// The fast-params vector's Argon2id output, through our call path (libargon2 agreed when
    /// the generator wrote it).
    #[test]
    fn the_fast_vector_password_derives_as_generated() {
        let out = derive_password_material(
            &SecretString::new(PASSWORD),
            &seq_array(0x80),
            &params(32, 1, 1),
            &KdfPolicy::INSECURE_FAST,
        )
        .unwrap();
        assert_eq!(
            hex(&out[..]),
            "f049865a2099fc22fe213398e42bc6d1cd3796f2567b556835bb01f8ecbd37dd"
        );
    }

    /// T22: release derivation time on this machine. Manual, on each CI OS through test.yml's
    /// `kdf_timing` dispatch; over about 1.5 s on any of them lowers RELEASE_DEFAULT to
    /// 128 MiB / t = 3 (never below the floor). Two numbers: end to end through
    /// `derive_password_material` — the allocation, the hash, the wipe, and in this test binary
    /// also the watching allocator's scan of the freed memory, so an upper bound of what a
    /// person pays, and the number T22 is decided on (release profile) — and the hash alone,
    /// into a working memory allocated before the clock starts.
    #[test]
    #[ignore = "timing only: test.yml's manual kdf_timing job runs it on the three CI OSes"]
    fn release_params_timing() {
        const EXPECTED: &str = "b0c4c318368316af56be9e1dff8602673bee13b27313e832169cff71368218ea";
        let _heavy = HEAVY_KDF.lock().unwrap_or_else(|e| e.into_inner());
        let release = Argon2idParams::RELEASE_DEFAULT;
        let start = Instant::now();
        let out = derive_password_material(
            &SecretString::new(PASSWORD),
            SALT,
            &release,
            &KdfPolicy::RELEASE,
        )
        .unwrap();
        let end_to_end = start.elapsed();
        assert_eq!(hex(&out[..]), EXPECTED);

        let argon = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(
                release.m_cost_kib,
                release.t_cost,
                release.p_cost,
                Some(MATERIAL_LEN),
            )
            .unwrap(),
        );
        let mut memory: Zeroizing<Vec<argon2::Block>> =
            Zeroizing::new(vec![argon2::Block::new(); argon.params().block_count()]);
        let mut material = Zeroizing::new([0u8; MATERIAL_LEN]);
        let start = Instant::now();
        argon
            .hash_password_into_with_memory(
                PASSWORD.as_bytes(),
                SALT,
                &mut material[..],
                &mut memory[..],
            )
            .unwrap();
        let hashing = start.elapsed();
        drop(memory);
        assert_eq!(hex(&material[..]), EXPECTED);
        println!(
            "release Argon2id (m = 262144 KiB, t = 3, p = 4, single-threaded): {} ms end to end, \
             {} ms hashing",
            end_to_end.as_millis(),
            hashing.as_millis()
        );
    }
}
