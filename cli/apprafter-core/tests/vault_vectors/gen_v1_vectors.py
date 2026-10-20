#!/usr/bin/env python3
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""Write AppRafter's frozen vault format v1 vectors (D.4a overview §3.7, T30) with libraries
independent of the Rust code: libsodium (PyNaCl) for XChaCha20-Poly1305, libargon2
(argon2-cffi) for Argon2id, Python's hmac/hashlib for HKDF-SHA256, and the BIP-350 reference
algorithm for Bech32 / Bech32m. Run once, under

    nix shell --impure --expr 'let p = import (builtins.getFlake "nixpkgs") {}; in
      p.python3.withPackages (ps: [ ps.pynacl ps.argon2-cffi ])' -c python3 gen_v1_vectors.py OUT

OUT must not exist. Every byte it writes is a function of the constants below: no randomness."""
import hashlib
import hmac
import json
import os
import struct
import sys

import argon2.low_level as a2
import nacl.bindings as na

OUT = sys.argv[1]

MAGIC = bytes([0x89, 0x41, 0x52, 0x56, 0x0D, 0x0A, 0x1A, 0x0A])
WRITER = b"apprafter 0.0.0-vector"
STORE_ID = b"0123456789abcdef"
SLOT_AAD_DOMAIN = b"apprafter-vault/v1/slot-wrap\x00"
INFO_KEK = b"apprafter-vault/v1/kek/"
INFO_CHECK = b"apprafter-vault/v1/key-check/"
KIND_PASSWORD, KIND_RECOVERY = 0x01, 0x02
PASSWORD = b"correct horse battery staple"
RELEASE = (262144, 3, 4)
FAST = (32, 1, 1)


def seq(start, n):
    return bytes((start + i) & 0xFF for i in range(n))


RECOVERY_KEY = seq(0x00, 32)
IDENTITY_BYTES = seq(0x20, 32)
DK = seq(0x40, 32)
VAULT_ID = seq(0x60, 16)
S1_ID, S1_SALT, S1_NONCE = seq(0x70, 16), seq(0x80, 16), seq(0x90, 24)
S2_ID, S2_SALT, S2_NONCE = seq(0xA8, 16), seq(0xB8, 16), seq(0xC8, 24)
PAYLOAD_NONCE = seq(0xE0, 24)
UNKNOWN_SLOT_ID = seq(0x30, 16)
UNKNOWN_SLOT_BODY = seq(0x00, 40)
PROD_TOKEN = b"A1b2C3d4" * 8
STAGING_TOKEN = b"s9T8u7V6" * 8

# --- Bech32 / Bech32m (BIP-173 / BIP-350 reference) -----------------------------------------
CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
BECH32, BECH32M = 1, 0x2BC830A3
RECOVERY_HRP = "apprafter-recovery-key-"


def polymod(values):
    gen = [0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3]
    chk = 1
    for v in values:
        b = chk >> 25
        chk = (chk & 0x1FFFFFF) << 5 ^ v
        for i in range(5):
            chk ^= gen[i] if ((b >> i) & 1) else 0
    return chk


def hrp_expand(hrp):
    return [ord(x) >> 5 for x in hrp] + [0] + [ord(x) & 31 for x in hrp]


def to5(data):
    acc = bits = 0
    out = []
    for v in data:
        acc = (acc << 8) | v
        bits += 8
        while bits >= 5:
            bits -= 5
            out.append((acc >> bits) & 31)
    if bits:
        out.append((acc << (5 - bits)) & 31)
    return out


def bech_encode(hrp, data8, const):
    d = to5(data8)
    pm = polymod(hrp_expand(hrp) + d + [0] * 6) ^ const
    chk = [(pm >> 5 * (5 - i)) & 31 for i in range(6)]
    return hrp + "1" + "".join(CHARSET[x] for x in d + chk)


def bech_valid(hrp, data_part, const):
    if any(c not in CHARSET for c in data_part):
        return False
    return polymod(hrp_expand(hrp) + [CHARSET.index(c) for c in data_part]) == const


def locate(d):
    """The group (1-based) of a mistyped recovery key's error: the one substitution that makes
    the checksum valid, else the one adjacent swap that does, else None."""
    subs = {i for i in range(len(d)) for c in CHARSET
            if c != d[i] and bech_valid(RECOVERY_HRP, d[:i] + c + d[i + 1:], BECH32M)}
    if len(subs) == 1:
        return subs.pop() // 5 + 1
    swaps = {i for i in range(len(d) - 1)
             if d[i] != d[i + 1] and bech_valid(RECOVERY_HRP, d[:i] + d[i + 1] + d[i] + d[i + 2:], BECH32M)}
    if not subs and len(swaps) == 1:
        return swaps.pop() // 5 + 1
    return None


IDENTITY = bech_encode("age-secret-key-", IDENTITY_BYTES, BECH32).upper()

# --- primitives ------------------------------------------------------------------------------


def hkdf(salt, ikm, info):
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    okm = hmac.new(prk, info + b"\x01", hashlib.sha256).digest()
    return prk, okm


def argon(params):
    m, t, p = params
    return a2.hash_secret_raw(secret=PASSWORD, salt=S1_SALT, time_cost=t, memory_cost=m,
                              parallelism=p, hash_len=32, type=a2.Type.ID, version=19)


def aead(key, nonce, aad, msg):
    return na.crypto_aead_xchacha20poly1305_ietf_encrypt(msg, aad, nonce, key)


def u16(v):
    return struct.pack("<H", v)


def u32(v):
    return struct.pack("<I", v)


# --- the format -------------------------------------------------------------------------------


def slot(kind, slot_id, salt, nonce, material, label, params=None):
    if kind == KIND_PASSWORD:
        m, t, p = params
        kdf_block = bytes([0x01]) + u32(m) + u32(t) + u32(p) + salt
    else:
        kdf_block = bytes([0x00]) + salt
    prk, kek = hkdf(salt, material, INFO_KEK + bytes([kind]))
    _, key_check = hkdf(salt, material, INFO_CHECK + bytes([kind]))
    aad = SLOT_AAD_DOMAIN + VAULT_ID + slot_id + bytes([kind]) + kdf_block
    wrapped = aead(kek, nonce, aad, DK)
    body = kdf_block + nonce + key_check + wrapped
    assert len(body) == (133 if kind == KIND_PASSWORD else 121)
    record = slot_id + bytes([kind]) + u16(len(body)) + body + bytes([len(label)]) + label
    trace = {"prk": prk.hex(), "kek": kek.hex(), "key_check": key_check.hex(),
             "slot_aad": aad.hex(), "wrapped_dk": wrapped.hex()}
    return u16(len(record)) + record, trace


def opaque_slot(kind, slot_id, body, label):
    record = slot_id + bytes([kind]) + u16(len(body)) + body + bytes([len(label)]) + label
    return u16(len(record)) + record


def tlv(entries):
    out = u16(len(entries))
    for key in sorted(entries):
        value = entries[key]
        out += bytes([len(key)]) + key + u16(len(value)) + value
    return out


def vault(slot_records, entries):
    header = MAGIC + u16(1) + bytes([len(WRITER)]) + WRITER + u16(0) + VAULT_ID
    header += bytes([len(STORE_ID)]) + STORE_ID + bytes([len(slot_records)]) + b"".join(slot_records)
    plain = tlv(entries)
    aad = header + PAYLOAD_NONCE + u32(len(plain) + 16)
    ct = aead(DK, PAYLOAD_NONCE, aad, plain)
    return aad + ct, plain, aad


def entries(extra=None):
    e = {b"age-identity": IDENTITY.encode(), b"settings/unlock-timeout-min": b"15",
         b"target/prod/hetzner-token": PROD_TOKEN, b"target/staging-1/hetzner-token": STAGING_TOKEN}
    e.update(extra or {})
    return e


def write(rel, data):
    path = os.path.join(OUT, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "xb") as f:
        f.write(data)


def chunks():
    """What `vault::create` draws, in order: vault id, data key, then per slot its id, salt and
    wrap nonce, then the payload nonce."""
    return [VAULT_ID.hex(), DK.hex(), S1_ID.hex(), S1_SALT.hex(), S1_NONCE.hex(),
            S2_ID.hex(), S2_SALT.hex(), S2_NONCE.hex(), PAYLOAD_NONCE.hex()]


def build(name, params, extra_entries=None, unknown_slot=False):
    material = argon(params)
    s1, t1 = slot(KIND_PASSWORD, S1_ID, S1_SALT, S1_NONCE, material, b"lock password", params)
    s2, t2 = slot(KIND_RECOVERY, S2_ID, S2_SALT, S2_NONCE, RECOVERY_KEY, b"recovery key")
    records = [s1, s2]
    if unknown_slot:
        records.append(opaque_slot(0x7F, UNKNOWN_SLOT_ID, UNKNOWN_SLOT_BODY, b"future slot"))
    e = entries(extra_entries)
    data, plain, aad = vault(records, e)
    write(f"{name}/vault", data)
    inputs = {
        "writer": WRITER.decode(), "store_id": STORE_ID.decode(),
        "password": PASSWORD.decode(),
        "params": {"m_cost_kib": params[0], "t_cost": params[1], "p_cost": params[2]},
        "recovery_key": RECOVERY_KEY.hex(),
        "random": chunks(),
        "entries": {k.decode(): v.hex() for k, v in sorted(e.items())},
        "argon2_output": material.hex(),
        "password_slot": t1, "recovery_slot": t2,
        "payload_aad": aad.hex(), "payload_plaintext": plain.hex(),
        "vault_sha256": hashlib.sha256(data).hexdigest(),
    }
    write(f"{name}/inputs.json", (json.dumps(inputs, indent=2, sort_keys=True) + "\n").encode())


def recovery_json():
    canonical = bech_encode(RECOVERY_HRP, RECOVERY_KEY, BECH32M).upper()
    data = canonical[len("APPRAFTER-RECOVERY-KEY-1"):]

    def grouped(d):
        return "-".join(d[i:i + 5] for i in range(0, len(d), 5))

    def sub(i, c):
        return data[:i] + c + data[i + 1:]

    groups = grouped(data)
    display = "APPRAFTER-RECOVERY-KEY-1\n" + groups
    other = bech_encode(RECOVERY_HRP, seq(0x01, 32), BECH32M).upper()
    swapped = data[:26] + data[27] + data[26] + data[28:]
    cases = [
        {"name": "canonical", "input": canonical, "expect": {"ok": True}},
        {"name": "lower_case", "input": canonical.lower(), "expect": {"ok": True}},
        {"name": "display", "input": display, "expect": {"ok": True}},
        {"name": "groups_only", "input": groups, "expect": {"ok": True}},
        {"name": "spaces", "input": groups.replace("-", " "), "expect": {"ok": True}},
        {"name": "mixed_case", "input": groups[:6].lower() + groups[6:], "expect": {"ok": True}},
        {"name": "padded", "input": "  " + display + " \n", "expect": {"ok": True}},
        {"name": "substitution", "input": grouped(sub(17, "H")), "expect": {"error": "checksum", "group": 4}},
        {"name": "transposition", "input": grouped(swapped), "expect": {"error": "checksum", "group": 6}},
        {"name": "dropped", "input": grouped(data[:40] + data[41:]), "expect": {"error": "wrong_length"}},
        {"name": "extra", "input": grouped(data + "Q"), "expect": {"error": "wrong_length"}},
        {"name": "one_for_l", "input": grouped(sub(7, "1")), "expect": {"error": "invalid_char", "position": 7}},
        {"name": "o_for_zero", "input": grouped(sub(50, "O")), "expect": {"error": "invalid_char", "position": 50}},
        {"name": "b_for_eight", "input": grouped(sub(45, "B")), "expect": {"error": "invalid_char", "position": 45}},
        {"name": "age_key", "input": IDENTITY, "expect": {"error": "wrong_prefix"}},
        {"name": "other_prefix", "input": "APPRAFTER-RECOVERY-1" + data, "expect": {"error": "wrong_prefix"}},
        {"name": "blank", "input": " \n\t ", "expect": {"error": "empty"}},
    ]
    assert data[17] == "G" and data[7] == "Y" and data[50] == "0" and data[45] == "8"
    for c in cases:
        if c["expect"].get("error") == "checksum":
            d = c["input"].replace("-", "").lower()
            assert not bech_valid(RECOVERY_HRP, d, BECH32M), c["name"]
            assert locate(d) == c["expect"]["group"], (c["name"], locate(d))
    doc = {"bytes": RECOVERY_KEY.hex(), "canonical": canonical, "display": display,
           "other_key": {"bytes": seq(0x01, 32).hex(), "canonical": other}, "cases": cases}
    write("recovery-key.json", (json.dumps(doc, indent=2) + "\n").encode())


STORE_YAML = b"""format: 1
secrets: locked
id: 0123456789abcdef
slots:
  - kind: password
    label: lock password
  - kind: recovery_key
    label: recovery key
unlock_timeout_min: 15
"""

if __name__ == "__main__":
    if os.path.exists(OUT):
        sys.exit(f"{OUT} exists: the v1 vectors are written once and never regenerated")
    build("password-recovery", RELEASE)
    build("fast-params", FAST)
    write("fast-params/store.yaml", STORE_YAML)
    build("unknown-slot", FAST, unknown_slot=True)
    build("unknown-entry", FAST, extra_entries={b"target/prod/future-thing": b"opaque-1",
                                                b"x-future/key": bytes([0xFF, 0xFE, 0x00, 0x01])})
    recovery_json()
    files = sorted(os.path.relpath(os.path.join(root, n), OUT)
                   for root, _, names in os.walk(OUT) for n in names)
    sums = "".join(f"{hashlib.sha256(open(os.path.join(OUT, f), 'rb').read()).hexdigest()}  {f}\n"
                   for f in files)
    write("SHA256SUMS", sums.encode())
    print(sums, end="")
