// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Known answers for the primitives the vault is built from, through the crates it uses, so a
//! crate swap that changes one output fails here first (D.4a T30): Argon2id (RFC 9106 §5.3),
//! XChaCha20-Poly1305 (draft-irtf-cfrg-xchacha-03 §A.3.1), HKDF-SHA256 (RFC 5869 A.1) and
//! Bech32m (BIP-350's valid and invalid strings).

use super::testutil::{hex, unhex};

#[test]
fn argon2id_rfc9106_section_5_3() {
    use argon2::{Algorithm, Argon2, AssociatedData, Block, ParamsBuilder, Version};
    let params = ParamsBuilder::new()
        .m_cost(32)
        .t_cost(3)
        .p_cost(4)
        .data(AssociatedData::new(&[0x04; 12]).unwrap())
        .build()
        .unwrap();
    let secret = [0x03; 8];
    let argon =
        Argon2::new_with_secret(&secret, Algorithm::Argon2id, Version::V0x13, params).unwrap();
    let mut memory = vec![Block::new(); argon.params().block_count()];
    let mut tag = [0u8; 32];
    argon
        .hash_password_into_with_memory(&[0x01; 32], &[0x02; 16], &mut tag, &mut memory[..])
        .unwrap();
    assert_eq!(
        hex(&tag),
        "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"
    );
}

#[test]
fn xchacha20poly1305_draft_irtf_cfrg_xchacha_03_a_3_1() {
    use chacha20poly1305::aead::{AeadInPlace, KeyInit};
    use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
    let key = unhex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
    let nonce = unhex("404142434445464748494a4b4c4d4e4f5051525354555657");
    let aad = unhex("50515253c0c1c2c3c4c5c6c7");
    let plaintext: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer you \
                             only one tip for the future, sunscreen would be it.";
    let ciphertext = "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb
                      731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452
                      2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9
                      21f9664c97637da9768812f615c68b13b52e";
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key));
    let mut buf = plaintext.to_vec();
    let tag = cipher
        .encrypt_in_place_detached(XNonce::from_slice(&nonce), &aad, &mut buf)
        .unwrap();
    assert_eq!(buf, unhex(ciphertext));
    assert_eq!(hex(&tag), "c0875924c1c7987947deafd8780acf49");
    cipher
        .decrypt_in_place_detached(XNonce::from_slice(&nonce), &aad, &mut buf, &tag)
        .unwrap();
    assert_eq!(buf, plaintext);
    // One flipped bit is refused, and the refused buffer keeps its ciphertext: the tag is
    // checked before the keystream is applied.
    let mut flipped = unhex(ciphertext);
    flipped[0] ^= 1;
    let before = flipped.clone();
    assert!(cipher
        .decrypt_in_place_detached(XNonce::from_slice(&nonce), &aad, &mut flipped, &tag)
        .is_err());
    assert_eq!(flipped, before);
}

#[test]
fn hkdf_sha256_rfc5869_test_case_1() {
    let ikm = unhex("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
    let salt = unhex("000102030405060708090a0b0c");
    let info = unhex("f0f1f2f3f4f5f6f7f8f9");
    let (prk, hkdf) = hkdf::Hkdf::<sha2::Sha256>::extract(Some(&salt[..]), &ikm);
    assert_eq!(
        hex(&prk),
        "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"
    );
    let mut okm = [0u8; 42];
    hkdf.expand(&info, &mut okm).unwrap();
    assert_eq!(
        hex(&okm),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
    );
}

#[test]
fn bech32m_bip350_valid_and_invalid_strings() {
    use bech32::primitives::decode::CheckedHrpstring;
    use bech32::Bech32m;
    let valid = [
        "A1LQFN3A",
        "a1lqfn3a",
        "an83characterlonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11sg7hg6",
        "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx",
        "11llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllludsr8",
        "split1checkupstagehandshakeupstreamerranterredcaperredlc445v",
        "?1v759aa",
    ];
    for s in valid {
        assert!(
            CheckedHrpstring::new::<Bech32m>(s).is_ok(),
            "BIP-350 lists {s:?} as valid Bech32m"
        );
    }
    let invalid = [
        "\u{20}1xj0phk",
        "\u{7f}1g6xzxy",
        "\u{80}1vctc34",
        "an84characterslonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1569pvx",
        "qyrz8wqd2c9m",
        "1qyrz8wqd2c9m",
        "y1b0jsk6g",
        "lt1igcx5c0",
        "in1muywd",
        "mm1crxm3i",
        "au1s5cgom",
        "M1VUXWEZ",
        "16plkw9",
        "1p2gdwpf",
    ];
    for s in invalid {
        assert!(
            CheckedHrpstring::new::<Bech32m>(s).is_err(),
            "BIP-350 lists {s:?} as invalid Bech32m"
        );
    }
}
