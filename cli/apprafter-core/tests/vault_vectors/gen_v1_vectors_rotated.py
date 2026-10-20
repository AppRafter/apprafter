#!/usr/bin/env python3
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""Write v1/rotated/ of AppRafter's frozen vault format v1 vectors (D.4b overview B6, B7, B63,
§4.5) with the independent implementation of gen_v1_vectors.py — libsodium (PyNaCl), libargon2
(argon2-cffi), Python's hashlib / hmac, the BIP-173 reference code — and the age files with
rage, never the Rust code:

- before/vault: a store of every slot shape a rotation meets — a lock password, this computer's
  account password (a Linux machine id), another computer's (a macOS one), an unknown slot (kind
  0x7f) and the recovery key — with D.4a's payload and an unknown entry;
- mid/vault: that store as vault::rotate writes it for `lock rekey` — a new data key, vault id
  and store key; this computer's account password and the recovery key kept under their own
  slot ids with a new salt and wrap nonce; a new lock password under a minted id; the old lock
  password, the other computer's slot and the unknown slot dropped; the old store key in
  `age-identity-retiring`;
- after/vault: mid as UnlockedVault::retire_identity writes it — the same data key, vault id
  and slots, the payload without `age-identity-retiring`, a new payload nonce;
- cache-old.age, cache-new.age: one plaintext sealed by rage to the old and to the new store
  key's recipient (the one randomised output — age draws an ephemeral key; their sums pin
  them, and their decryption is checked here and in the Rust).

Run once, beside gen_v1_vectors.py, under

    nix shell --impure --expr 'let p = import (builtins.getFlake "nixpkgs") {}; in
      p.buildEnv { name = "d4b-b2-gen"; paths = [
        (p.python3.withPackages (ps: [ ps.pynacl ps.argon2-cffi ])) p.rage ]; }' -c \\
      python3 gen_v1_vectors_rotated.py OUT FROZEN

OUT must not exist. FROZEN is the committed v1/ directory, which this script only reads: it
first rebuilds v1/fast-params/vault with gen_v1_vectors.py's functions and v1/os-password/vault
with this script's own, and stops unless both match. Every byte of the vaults and of
inputs.json is a function of the constants below. It prints the SHA-256 of each file written."""
import hashlib
import json
import os
import struct
import subprocess
import sys
import tempfile

import nacl.bindings as na

import gen_v1_vectors as d4a  # its primitives and constants; it writes nothing when imported

OUT, FROZEN = sys.argv[1], sys.argv[2]

KIND_OS_PASSWORD, KIND_UNKNOWN = 0x03, 0x7F
RETIRING = b"age-identity-retiring"
MACHINE_SECRET_DOMAIN = b"apprafter-vault/v1/machine-secret\x00"
MACHINE_TAG_DOMAIN = b"apprafter-vault/v1/machine\x00"
ACCOUNT_PASSWORD = b"hunter2 is not a password"
NEW_PASSWORD = b"a new lock password for the rotated vault"
# B76: Linux, the 32 lower-case hex characters of /etc/machine-id without its line end; macOS,
# the 16 raw bytes gethostuuid() fills (D.4b-b1's vectors use the same two).
MACHINE_LINUX = b"5f0d6a3e0c2b4f6a8d9e1b2c3d4e5f60"
MACHINE_MAC = bytes.fromhex("3c1e9a7b2d4f4e8a9b0c1d2e3f405162")
LABEL_PASSWORD, LABEL_RECOVERY = b"lock password", b"recovery key"
LABEL_LINUX = b"account password of walker on box-a"
LABEL_MAC = b"account password of walker on box-b"
LABEL_UNKNOWN = b"future slot"
ENROLLED_LINUX, ENROLLED_MAC = 1760000000, 1760100000
EXTRA = {b"x-future/key": bytes([0xFF, 0xFE, 0x00, 0x01])}
CACHE_PLAINTEXT = b"apiVersion: v1\nkind: Config\n# v1/rotated: one cache, sealed to each store key\n"

# before/: D.4a's vault id, data key and payload nonce; per slot its id, salt and wrap nonce.
B_PW = (d4a.S1_ID, d4a.S1_SALT, d4a.S1_NONCE)
B_L = (d4a.seq(0x50, 16), d4a.seq(0x30, 16), d4a.seq(0xF0, 24))
B_M = (d4a.seq(0x01, 16), d4a.seq(0x11, 16), d4a.seq(0x21, 24))
B_R = (d4a.S2_ID, d4a.S2_SALT, d4a.S2_NONCE)
B_U_ID = d4a.UNKNOWN_SLOT_ID
# The rotation: the ids minted at plan time (B6, B80), then vault::rotate's draws in its order —
# the data key, the store key's 32 bytes, per kept slot in the current header's order its salt
# and wrap nonce, per new slot the same, then the payload nonce.
ROT_VAULT_ID, ROT_PW_ID = d4a.seq(0xC0, 16), d4a.seq(0xD0, 16)
ROT_DK, NEW_IDENTITY_BYTES = d4a.seq(0x10, 32), d4a.seq(0xA0, 32)
K_L_SALT, K_L_NONCE = d4a.seq(0x41, 16), d4a.seq(0x51, 24)
K_R_SALT, K_R_NONCE = d4a.seq(0x69, 16), d4a.seq(0x79, 24)
N_PW_SALT, N_PW_NONCE = d4a.seq(0x91, 16), d4a.seq(0xA1, 24)
MID_NONCE = d4a.seq(0xB9, 24)
# retire_identity's one draw: the payload nonce.
AFTER_NONCE = d4a.seq(0xD1, 24)
# age's identity text: Bech32 (BIP 173) under the HRP age-secret-key-, upper case.
NEW_IDENTITY = d4a.bech_encode("age-secret-key-", NEW_IDENTITY_BYTES, d4a.BECH32).upper()


def u64(v):
    return struct.pack("<Q", v)


def hexed(entries):
    return {k.decode(): v.hex() for k, v in sorted(entries.items())}


def machine_secret(machine):
    return hashlib.sha256(MACHINE_SECRET_DOMAIN + machine).digest()


def machine_tag(slot_id, machine):
    return hashlib.sha256(MACHINE_TAG_DOMAIN + slot_id + machine).digest()[:8]


def argon(password, salt):
    m, t, p = d4a.FAST
    return d4a.a2.hash_secret_raw(secret=password, salt=salt, time_cost=t, memory_cost=m,
                                  parallelism=p, hash_len=32, type=d4a.a2.Type.ID, version=19)


def record(slot_id, kind, body, label):
    rec = slot_id + bytes([kind]) + d4a.u16(len(body)) + body + bytes([len(label)]) + label
    return d4a.u16(len(rec)) + rec


def wrap(dk, vault_id, slot_id, kind, kdf_block, salt, nonce, material):
    """A slot's body after its kdf block, wrapping the data key dk, and its trace."""
    prk, kek = d4a.hkdf(salt, material, d4a.INFO_KEK + bytes([kind]))
    _, key_check = d4a.hkdf(salt, material, d4a.INFO_CHECK + bytes([kind]))
    aad = d4a.SLOT_AAD_DOMAIN + vault_id + slot_id + bytes([kind]) + kdf_block
    wrapped = d4a.aead(kek, nonce, aad, dk)
    trace = {"prk": prk.hex(), "kek": kek.hex(), "key_check": key_check.hex(),
             "slot_aad": aad.hex(), "wrapped_dk": wrapped.hex()}
    return kdf_block + nonce + key_check + wrapped, trace


def fast_block(salt):
    m, t, p = d4a.FAST
    return bytes([0x01]) + d4a.u32(m) + d4a.u32(t) + d4a.u32(p) + salt


def password_slot(dk, vault_id, ids, password, label):
    slot_id, salt, nonce = ids
    argon_out = argon(password, salt)
    body, trace = wrap(dk, vault_id, slot_id, d4a.KIND_PASSWORD, fast_block(salt), salt, nonce,
                       argon_out)
    assert len(body) == 133
    trace.update({"kind": d4a.KIND_PASSWORD, "label": label.decode(),
                  "argon2_output": argon_out.hex()})
    return record(slot_id, d4a.KIND_PASSWORD, body, label), trace


def os_slot(dk, vault_id, ids, enrolled_at, machine, name, label):
    slot_id, salt, nonce = ids
    tag = machine_tag(slot_id, machine)
    kdf_block = fast_block(salt) + u64(enrolled_at) + tag
    assert len(kdf_block) == 45
    argon_out = argon(ACCOUNT_PASSWORD, salt)
    secret = machine_secret(machine)
    material = argon_out + secret
    body, trace = wrap(dk, vault_id, slot_id, KIND_OS_PASSWORD, kdf_block, salt, nonce, material)
    assert len(body) == 149
    trace.update({"kind": KIND_OS_PASSWORD, "label": label.decode(), "machine": name,
                  "enrolled_at": enrolled_at, "machine_tag": tag.hex(),
                  "argon2_output": argon_out.hex(), "machine_secret": secret.hex(),
                  "material": material.hex()})
    return record(slot_id, KIND_OS_PASSWORD, body, label), trace


def recovery_slot(dk, vault_id, ids):
    slot_id, salt, nonce = ids
    body, trace = wrap(dk, vault_id, slot_id, d4a.KIND_RECOVERY, bytes([0x00]) + salt, salt,
                       nonce, d4a.RECOVERY_KEY)
    assert len(body) == 121
    trace.update({"kind": d4a.KIND_RECOVERY, "label": LABEL_RECOVERY.decode()})
    return record(slot_id, d4a.KIND_RECOVERY, body, LABEL_RECOVERY), trace


def vault_bytes(dk, vault_id, records, entries, nonce):
    header = (d4a.MAGIC + d4a.u16(1) + bytes([len(d4a.WRITER)]) + d4a.WRITER + d4a.u16(0)
              + vault_id)
    header += bytes([len(d4a.STORE_ID)]) + d4a.STORE_ID + bytes([len(records)]) + b"".join(records)
    plain = d4a.tlv(entries)
    aad = header + nonce + d4a.u32(len(plain) + 16)
    return aad + d4a.aead(dk, nonce, aad, plain), plain, aad


def payload_info(data, plain, aad, entries):
    return {"entries": hexed(entries), "payload_aad": aad.hex(),
            "payload_plaintext": plain.hex(), "vault_sha256": hashlib.sha256(data).hexdigest()}


def frozen(rel):
    with open(os.path.join(FROZEN, rel), "rb") as f:
        return f.read()


def check_frozen():
    """This environment, gen_v1_vectors.py's functions and this script's own still write the
    frozen v1/fast-params/vault and D.4b-b1's v1/os-password/vault."""
    material = d4a.argon(d4a.FAST)
    s1, _ = d4a.slot(d4a.KIND_PASSWORD, d4a.S1_ID, d4a.S1_SALT, d4a.S1_NONCE, material,
                     LABEL_PASSWORD, d4a.FAST)
    s2, _ = d4a.slot(d4a.KIND_RECOVERY, d4a.S2_ID, d4a.S2_SALT, d4a.S2_NONCE, d4a.RECOVERY_KEY,
                     LABEL_RECOVERY)
    data, _, _ = d4a.vault([s1, s2], d4a.entries())
    assert data == frozen("fast-params/vault"), "gen_v1_vectors.py no longer writes fast-params"
    pw, _ = password_slot(d4a.DK, d4a.VAULT_ID, B_PW, d4a.PASSWORD, LABEL_PASSWORD)
    rec, _ = recovery_slot(d4a.DK, d4a.VAULT_ID, B_R)
    assert (pw, rec) == (s1, s2), "this script's slots differ from gen_v1_vectors.py's"
    linux, _ = os_slot(d4a.DK, d4a.VAULT_ID, (d4a.S1_ID, d4a.S1_SALT, d4a.S1_NONCE),
                       ENROLLED_LINUX, MACHINE_LINUX, "linux", LABEL_LINUX)
    mac, _ = os_slot(d4a.DK, d4a.VAULT_ID, (d4a.seq(0x50, 16), d4a.seq(0x30, 16),
                                            d4a.seq(0xF0, 24)),
                     ENROLLED_MAC, MACHINE_MAC, "mac", LABEL_MAC)
    data, _, _ = vault_bytes(d4a.DK, d4a.VAULT_ID, [linux, mac, rec], d4a.entries(),
                             d4a.PAYLOAD_NONCE)
    assert data == frozen("os-password/vault"), "this script no longer writes os-password"


def before():
    dk, vid = d4a.DK, d4a.VAULT_ID
    pw, t_pw = password_slot(dk, vid, B_PW, d4a.PASSWORD, LABEL_PASSWORD)
    linux, t_l = os_slot(dk, vid, B_L, ENROLLED_LINUX, MACHINE_LINUX, "linux", LABEL_LINUX)
    mac, t_m = os_slot(dk, vid, B_M, ENROLLED_MAC, MACHINE_MAC, "mac", LABEL_MAC)
    unknown = record(B_U_ID, KIND_UNKNOWN, d4a.UNKNOWN_SLOT_BODY, LABEL_UNKNOWN)
    t_u = {"kind": KIND_UNKNOWN, "label": LABEL_UNKNOWN.decode(),
           "body": d4a.UNKNOWN_SLOT_BODY.hex()}
    rec, t_r = recovery_slot(dk, vid, B_R)
    entries = d4a.entries(EXTRA)
    data, plain, aad = vault_bytes(dk, vid, [pw, linux, mac, unknown, rec], entries,
                                   d4a.PAYLOAD_NONCE)
    assert len(data) == 1252, len(data)
    info = payload_info(data, plain, aad, entries)
    info.update({"vault_id": vid.hex(), "data_key": dk.hex(),
                 "slots": [t_pw, t_l, t_m, t_u, t_r]})
    return data, info


def rotated_slots():
    """mid's (and after's) slots: the kept ones in before's header order, then the new one."""
    dk, vid = ROT_DK, ROT_VAULT_ID
    linux, t_l = os_slot(dk, vid, (B_L[0], K_L_SALT, K_L_NONCE), ENROLLED_LINUX, MACHINE_LINUX,
                         "linux", LABEL_LINUX)
    rec, t_r = recovery_slot(dk, vid, (B_R[0], K_R_SALT, K_R_NONCE))
    pw, t_pw = password_slot(dk, vid, (ROT_PW_ID, N_PW_SALT, N_PW_NONCE), NEW_PASSWORD,
                             LABEL_PASSWORD)
    return [linux, rec, pw], [t_l, t_r, t_pw]


def mid_entries():
    entries = d4a.entries(EXTRA)
    entries[RETIRING] = entries[b"age-identity"]  # the store key it retires, byte for byte
    entries[b"age-identity"] = NEW_IDENTITY.encode()
    return entries


def after_entries():
    entries = d4a.entries(EXTRA)
    entries[b"age-identity"] = NEW_IDENTITY.encode()
    return entries


def recipient(identity_bytes):
    """age's recipient: X25519 of the clamped scalar (libsodium clamps), Bech32 under `age`."""
    return d4a.bech_encode("age", na.crypto_scalarmult_base(identity_bytes), d4a.BECH32)


def rage_seal(to):
    return subprocess.run(["rage", "--armor", "--recipient", to], input=CACHE_PLAINTEXT,
                          capture_output=True, check=True).stdout


def rage_opens(armored, identity):
    """Whether rage opens `armored` with the identity text `identity`, to the plaintext."""
    # Beside OUT (the run's scratch), never the system temp dir (critic round, 16).
    with tempfile.TemporaryDirectory(dir=os.path.dirname(os.path.abspath(OUT))) as tmp:
        key = os.path.join(tmp, "identity.txt")
        with open(key, "w") as f:
            f.write(identity + "\n")
        r = subprocess.run(["rage", "--decrypt", "--identity", key], input=armored,
                           capture_output=True)
    return r.returncode == 0 and r.stdout == CACHE_PLAINTEXT


def write(rel, data):
    path = os.path.join(OUT, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "xb") as f:
        f.write(data)


if __name__ == "__main__":
    if os.path.exists(OUT):
        sys.exit(f"{OUT} exists: the v1 vectors are written once and never regenerated")
    check_frozen()
    b_data, b_info = before()
    records, traces = rotated_slots()
    m_data, m_plain, m_aad = vault_bytes(ROT_DK, ROT_VAULT_ID, records, mid_entries(), MID_NONCE)
    assert len(m_data) == 1071, len(m_data)
    a_data, a_plain, a_aad = vault_bytes(ROT_DK, ROT_VAULT_ID, records, after_entries(),
                                         AFTER_NONCE)
    assert len(a_data) == 973, len(a_data)
    old_to, new_to = recipient(d4a.IDENTITY_BYTES), recipient(NEW_IDENTITY_BYTES)
    cache_old, cache_new = rage_seal(old_to), rage_seal(new_to)
    assert rage_opens(cache_old, d4a.IDENTITY) and not rage_opens(cache_old, NEW_IDENTITY)
    assert rage_opens(cache_new, NEW_IDENTITY) and not rage_opens(cache_new, d4a.IDENTITY)
    write("rotated/before/vault", b_data)
    write("rotated/mid/vault", m_data)
    write("rotated/after/vault", a_data)
    write("rotated/cache-old.age", cache_old)
    write("rotated/cache-new.age", cache_new)
    inputs = {
        "writer": d4a.WRITER.decode(), "store_id": d4a.STORE_ID.decode(),
        "password": d4a.PASSWORD.decode(), "new_password": NEW_PASSWORD.decode(),
        "account_password": ACCOUNT_PASSWORD.decode(),
        "params": {"m_cost_kib": d4a.FAST[0], "t_cost": d4a.FAST[1], "p_cost": d4a.FAST[2]},
        "machine_ids": {"linux": MACHINE_LINUX.decode(), "mac": MACHINE_MAC.hex()},
        "recovery_key": d4a.RECOVERY_KEY.hex(),
        "identities": {"old": d4a.IDENTITY, "new": NEW_IDENTITY,
                       "old_bytes": d4a.IDENTITY_BYTES.hex(),
                       "new_bytes": NEW_IDENTITY_BYTES.hex()},
        "recipients": {"old": old_to, "new": new_to},
        "before": b_info,
        "rotation": {
            "vault_id": ROT_VAULT_ID.hex(), "new_slots": [ROT_PW_ID.hex()],
            "kept": [B_L[0].hex(), B_R[0].hex()],
            "dropped": [B_PW[0].hex(), B_M[0].hex(), B_U_ID.hex()],
            "random": [c.hex() for c in (ROT_DK, NEW_IDENTITY_BYTES, K_L_SALT, K_L_NONCE,
                                         K_R_SALT, K_R_NONCE, N_PW_SALT, N_PW_NONCE,
                                         MID_NONCE)],
        },
        "slots": traces,
        "mid": payload_info(m_data, m_plain, m_aad, mid_entries()),
        "after": {**payload_info(a_data, a_plain, a_aad, after_entries()),
                  "random": [AFTER_NONCE.hex()]},
        "cache": {"plaintext": CACHE_PLAINTEXT.decode(), "old": "cache-old.age",
                  "new": "cache-new.age"},
    }
    write("rotated/inputs.json", (json.dumps(inputs, indent=2, sort_keys=True) + "\n").encode())
    files = sorted(os.path.relpath(os.path.join(root, n), OUT)
                   for root, _, names in os.walk(OUT) for n in names)
    for rel in files:
        with open(os.path.join(OUT, rel), "rb") as f:
            print(f"{hashlib.sha256(f.read()).hexdigest()}  {rel}")
