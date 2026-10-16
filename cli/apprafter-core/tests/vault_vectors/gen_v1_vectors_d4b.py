#!/usr/bin/env python3
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""Write D.4b's additions to AppRafter's frozen vault format v1 vectors (D.4b overview B7, B54,
B57, B76) with the independent implementation of gen_v1_vectors.py — libsodium (PyNaCl),
libargon2 (argon2-cffi), Python's hashlib / hmac — never the Rust code:

- v1/os-password/: two account-password slots (kind 0x03), one bound to a Linux machine id (the
  32 lower-case hex characters of /etc/machine-id), one to a macOS one (the 16 raw bytes of
  gethostuuid()), and the recovery slot, as vault::create writes them from its draws;
- v1/created-with/: an account-password first slot and the recovery slot, every id fixed
  beforehand, as vault::create_with writes them.

Run once, beside gen_v1_vectors.py, under

    nix shell --impure --expr 'let p = import (builtins.getFlake "nixpkgs") {}; in
      p.python3.withPackages (ps: [ ps.pynacl ps.argon2-cffi ])' -c \\
      python3 gen_v1_vectors_d4b.py OUT FROZEN

OUT must not exist. FROZEN is the committed v1/ directory, which this script only reads: it first
rebuilds v1/fast-params/vault with gen_v1_vectors.py's functions and stops unless the bytes
match. Every byte it writes is a function of the constants below: no randomness. It prints the
SHA-256 of each file written."""
import hashlib
import json
import os
import struct
import sys

import gen_v1_vectors as d4a  # its primitives and constants; it writes nothing when imported

OUT, FROZEN = sys.argv[1], sys.argv[2]

KIND_OS_PASSWORD = 0x03
MACHINE_SECRET_DOMAIN = b"apprafter-vault/v1/machine-secret\x00"
MACHINE_TAG_DOMAIN = b"apprafter-vault/v1/machine\x00"
ACCOUNT_PASSWORD = b"hunter2 is not a password"
# B76: Linux, the 32 lower-case hex characters of /etc/machine-id without its line end; macOS,
# the 16 raw bytes gethostuuid() fills.
MACHINE_LINUX = b"5f0d6a3e0c2b4f6a8d9e1b2c3d4e5f60"
MACHINE_MAC = bytes.fromhex("3c1e9a7b2d4f4e8a9b0c1d2e3f405162")
LABEL_LINUX = b"account password of walker on box-a"
LABEL_MAC = b"account password of walker on box-b"
LABEL_RECOVERY = b"recovery key"
ENROLLED_LINUX, ENROLLED_MAC, ENROLLED_CW = 1760000000, 1760100000, 1760200000
# os-password/: vault::create's draws, in its order — the vault id, the data key, then per slot
# its id, salt and wrap nonce, then the payload nonce (D.4a's order).
L_ID, L_SALT, L_NONCE = d4a.S1_ID, d4a.S1_SALT, d4a.S1_NONCE
M_ID, M_SALT, M_NONCE = d4a.seq(0x50, 16), d4a.seq(0x30, 16), d4a.seq(0xF0, 24)
R_ID, R_SALT, R_NONCE = d4a.S2_ID, d4a.S2_SALT, d4a.S2_NONCE
# created-with/: the ids minted at plan time (B57), then vault::create_with's draws — the data
# key, per slot its salt and wrap nonce, the payload nonce.
CW_VAULT_ID, CW_L_ID, CW_R_ID = d4a.seq(0x11, 16), d4a.seq(0x21, 16), d4a.seq(0x31, 16)


def u64(v):
    return struct.pack("<Q", v)


def machine_secret(machine):
    return hashlib.sha256(MACHINE_SECRET_DOMAIN + machine).digest()


def machine_tag(slot_id, machine):
    return hashlib.sha256(MACHINE_TAG_DOMAIN + slot_id + machine).digest()[:8]


def argon(password, salt, params):
    m, t, p = params
    return d4a.a2.hash_secret_raw(secret=password, salt=salt, time_cost=t, memory_cost=m,
                                  parallelism=p, hash_len=32, type=d4a.a2.Type.ID, version=19)


def record(slot_id, kind, body, label):
    rec = slot_id + bytes([kind]) + d4a.u16(len(body)) + body + bytes([len(label)]) + label
    return d4a.u16(len(rec)) + rec


def wrap(vault_id, slot_id, kind, kdf_block, salt, nonce, material):
    prk, kek = d4a.hkdf(salt, material, d4a.INFO_KEK + bytes([kind]))
    _, key_check = d4a.hkdf(salt, material, d4a.INFO_CHECK + bytes([kind]))
    aad = d4a.SLOT_AAD_DOMAIN + vault_id + slot_id + bytes([kind]) + kdf_block
    wrapped = d4a.aead(kek, nonce, aad, d4a.DK)
    trace = {"prk": prk.hex(), "kek": kek.hex(), "key_check": key_check.hex(),
             "slot_aad": aad.hex(), "wrapped_dk": wrapped.hex()}
    return kdf_block + nonce + key_check + wrapped, trace


def os_slot(vault_id, slot_id, salt, nonce, enrolled_at, machine, name, label):
    m, t, p = d4a.FAST
    tag = machine_tag(slot_id, machine)
    kdf_block = (bytes([0x01]) + d4a.u32(m) + d4a.u32(t) + d4a.u32(p) + salt + u64(enrolled_at)
                 + tag)
    assert len(kdf_block) == 45
    argon_out = argon(ACCOUNT_PASSWORD, salt, d4a.FAST)
    secret = machine_secret(machine)
    material = argon_out + secret
    body, trace = wrap(vault_id, slot_id, KIND_OS_PASSWORD, kdf_block, salt, nonce, material)
    assert len(body) == 149
    trace.update({"kind": KIND_OS_PASSWORD, "label": label.decode(), "machine": name,
                  "enrolled_at": enrolled_at, "machine_tag": tag.hex(),
                  "argon2_output": argon_out.hex(), "machine_secret": secret.hex(),
                  "material": material.hex()})
    return record(slot_id, KIND_OS_PASSWORD, body, label), trace


def recovery_slot(vault_id, slot_id, salt, nonce):
    body, trace = wrap(vault_id, slot_id, d4a.KIND_RECOVERY, bytes([0x00]) + salt, salt, nonce,
                       d4a.RECOVERY_KEY)
    assert len(body) == 121
    trace.update({"kind": d4a.KIND_RECOVERY, "label": LABEL_RECOVERY.decode()})
    return record(slot_id, d4a.KIND_RECOVERY, body, LABEL_RECOVERY), trace


def vault_bytes(vault_id, records):
    header = (d4a.MAGIC + d4a.u16(1) + bytes([len(d4a.WRITER)]) + d4a.WRITER + d4a.u16(0)
              + vault_id)
    header += bytes([len(d4a.STORE_ID)]) + d4a.STORE_ID + bytes([len(records)]) + b"".join(records)
    plain = d4a.tlv(d4a.entries())
    aad = header + d4a.PAYLOAD_NONCE + d4a.u32(len(plain) + 16)
    return aad + d4a.aead(d4a.DK, d4a.PAYLOAD_NONCE, aad, plain), plain, aad


def write(rel, data):
    path = os.path.join(OUT, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "xb") as f:
        f.write(data)


def emit(name, data, plain, aad, extra):
    write(f"{name}/vault", data)
    inputs = {
        "writer": d4a.WRITER.decode(), "store_id": d4a.STORE_ID.decode(),
        "account_password": ACCOUNT_PASSWORD.decode(),
        "params": {"m_cost_kib": d4a.FAST[0], "t_cost": d4a.FAST[1], "p_cost": d4a.FAST[2]},
        "machine_ids": {"linux": MACHINE_LINUX.decode(), "mac": MACHINE_MAC.hex()},
        "recovery_key": d4a.RECOVERY_KEY.hex(),
        "entries": {k.decode(): v.hex() for k, v in sorted(d4a.entries().items())},
        "payload_aad": aad.hex(), "payload_plaintext": plain.hex(),
        "vault_sha256": hashlib.sha256(data).hexdigest(),
    }
    inputs.update(extra)
    write(f"{name}/inputs.json", (json.dumps(inputs, indent=2, sort_keys=True) + "\n").encode())


def os_password():
    linux, t1 = os_slot(d4a.VAULT_ID, L_ID, L_SALT, L_NONCE, ENROLLED_LINUX, MACHINE_LINUX,
                        "linux", LABEL_LINUX)
    mac, t2 = os_slot(d4a.VAULT_ID, M_ID, M_SALT, M_NONCE, ENROLLED_MAC, MACHINE_MAC, "mac",
                      LABEL_MAC)
    rec, t3 = recovery_slot(d4a.VAULT_ID, R_ID, R_SALT, R_NONCE)
    data, plain, aad = vault_bytes(d4a.VAULT_ID, [linux, mac, rec])
    assert len(data) == 992
    draws = [d4a.VAULT_ID, d4a.DK, L_ID, L_SALT, L_NONCE, M_ID, M_SALT, M_NONCE, R_ID, R_SALT,
             R_NONCE, d4a.PAYLOAD_NONCE]
    emit("os-password", data, plain, aad,
         {"random": [c.hex() for c in draws], "slots": [t1, t2, t3]})


def created_with():
    linux, t1 = os_slot(CW_VAULT_ID, CW_L_ID, L_SALT, L_NONCE, ENROLLED_CW, MACHINE_LINUX,
                        "linux", LABEL_LINUX)
    rec, t2 = recovery_slot(CW_VAULT_ID, CW_R_ID, R_SALT, R_NONCE)
    data, plain, aad = vault_bytes(CW_VAULT_ID, [linux, rec])
    assert len(data) == 786
    draws = [d4a.DK, L_SALT, L_NONCE, R_SALT, R_NONCE, d4a.PAYLOAD_NONCE]
    emit("created-with", data, plain, aad, {
        "ids": {"vault_id": CW_VAULT_ID.hex(), "slots": [CW_L_ID.hex(), CW_R_ID.hex()]},
        "random": [c.hex() for c in draws], "slots": [t1, t2]})


def check_frozen():
    """This environment and gen_v1_vectors.py's functions still write v1/fast-params/vault."""
    material = d4a.argon(d4a.FAST)
    s1, _ = d4a.slot(d4a.KIND_PASSWORD, d4a.S1_ID, d4a.S1_SALT, d4a.S1_NONCE, material,
                     b"lock password", d4a.FAST)
    s2, _ = d4a.slot(d4a.KIND_RECOVERY, d4a.S2_ID, d4a.S2_SALT, d4a.S2_NONCE,
                     d4a.RECOVERY_KEY, b"recovery key")
    data, _, _ = d4a.vault([s1, s2], d4a.entries())
    with open(os.path.join(FROZEN, "fast-params", "vault"), "rb") as f:
        assert data == f.read(), "gen_v1_vectors.py no longer writes v1/fast-params/vault here"


if __name__ == "__main__":
    if os.path.exists(OUT):
        sys.exit(f"{OUT} exists: the v1 vectors are written once and never regenerated")
    check_frozen()
    os_password()
    created_with()
    files = sorted(os.path.relpath(os.path.join(root, n), OUT)
                   for root, _, names in os.walk(OUT) for n in names)
    for rel in files:
        with open(os.path.join(OUT, rel), "rb") as f:
            print(f"{hashlib.sha256(f.read()).hexdigest()}  {rel}")
