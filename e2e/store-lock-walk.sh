#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# store-lock-walk.sh — the target-store lock end to end, on kind (D.4a,
# ADR 0068).
#
# Legs, each printing `ok: <what>`:
#   files    the scratch store in files: the cached kind kubeconfig reads with
#            the walk's own age key and reaches the cluster.
#   lock on  at a terminal of the walk's own: a 14-character password (the
#            15-character advice, kept with `y`), the recovery key typed back
#            whole after one mistyped try; afterwards no token in clear, the
#            cache re-sealed to the store's own key, the vault owner-only.
#   window   one terminal, a shell leading its session: `unlock` (a wrong
#            secret asked again), then `kubeconfig`, `kubeconfig </dev/null`,
#            `doctor --no-ping` (its cluster check through the window) and
#            `lock status`, none asking again; `lock now` closes the window.
#   others   another terminal is asked (Ctrl-C stops it); a command with no
#            terminal is refused `store_locked`; HCLOUD_TOKEN and
#            APPRAFTER_AGE_KEY are refused `override_refused`; `target show`
#            names the vault; closing a terminal ends its window.
#   timeout  `lock on --timeout 15` on a locked store asks, and sets 15 min.
#   lock off from a second terminal while the first holds a window: that
#            window ends within the holder's store watch; the token and the
#            cache are back in files, the cache under the walk's age key.
#
# Every CLI call runs on a target store of its own (e2e_isolate_store), at a
# pseudo-terminal this walk owns or with no terminal at all: never the
# person's terminal, store, keyring or holder. The cluster is a bare kind
# cluster. Nothing reaches a provider (APPRAFTER_HCLOUD_BASE_URL is a closed
# port).
#
# Usage, inside `nix develop` (kind, kubectl, age, python3), with the CLI
# built first (in cli/: cargo build --release -p apprafter):
#   APPRAFTER_BIN=<that binary> bash e2e/store-lock-walk.sh 2>&1 | tee walk.log
# Judge the run by its log: exactly one line `WALK GREEN` (kind's cleanup lines
# follow it, so it is not the last line), and no `FAILED: …` or traceback; a
# surprise prints `FAILED: …` with the terminal so far and stops.
# APPRAFTER_E2E_KEEP=1 keeps the cluster and the sandbox for a look.
set -euo pipefail
# shellcheck source=e2e/lib.sh
source "$(dirname "$0")/lib.sh"

CLUSTER=apprafter-lock-walk
WALK_KUBECONFIG=""

fail() { printf 'FAILED: %s\n' "$*"; exit 1; }

for tool in python3 kubectl age age-keygen pgrep realpath; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is not on PATH (run the walk inside nix develop)"
done
[ -n "${APPRAFTER_BIN:-}" ] || fail "set APPRAFTER_BIN to the release build of the CLI"
APPRAFTER_BIN="$(realpath "$APPRAFTER_BIN")"
export APPRAFTER_BIN
unset HCLOUD_TOKEN SSH_AUTH_SOCK

e2e_isolate_store
APPRAFTER="$(apprafter_bin)"
export APPRAFTER_HCLOUD_BASE_URL=http://127.0.0.1:1

cleanup() {
    local rc=$?
    if [ "${APPRAFTER_E2E_KEEP:-0}" = 1 ]; then
        printf 'kept: cluster %s, sandbox %s\n' "$CLUSTER" "$E2E_SANDBOX_DIR"
    else
        k3d_down "$CLUSTER"
        if [ -n "$WALK_KUBECONFIG" ]; then rm -f "$WALK_KUBECONFIG"; fi
        e2e_isolation_cleanup
    fi
    exit "$rc"
}
trap cleanup EXIT

phase "kind cluster"
k3d_up "$CLUSTER"
WALK_KUBECONFIG="$KUBECONFIG"
# Capture, then match: no grep -q under pipefail (memory trap 1).
nodes="$(kubectl --kubeconfig "$WALK_KUBECONFIG" get nodes -o name || true)"
case "$nodes" in node/*) ;; *) fail "the kind cluster has no node" ;; esac

phase "the walk's store, in files"
"$APPRAFTER" target add walk --provider hetzner-cloud --token "$(printf 'w%.0s' $(seq 64))" \
    --region nbg1 --no-ping --no-interactive || fail "target add"
# The key alone, as AppRafter writes one: its parser takes no comment lines.
age-keygen 2>/dev/null | grep '^AGE-SECRET-KEY-' >"$APPRAFTER_AGE_KEY"
chmod 600 "$APPRAFTER_AGE_KEY"
sealed="$(age -a -r "$(age-keygen -y "$APPRAFTER_AGE_KEY")" <"$WALK_KUBECONFIG")"
mkdir -p "$APPRAFTER_CONFIG_DIR/state/walk/.apprafter"
python3 - "$APPRAFTER_CONFIG_DIR/state/walk/.apprafter/state.json" "$sealed" <<'PY'
import json, sys
with open(sys.argv[1], "w") as f:
    json.dump({"hetzner_cloud": {"server_id": 1, "server_name": "walk-node",
                                 "kubeconfig_age": sys.argv[2] + "\n"}}, f)
PY
"$APPRAFTER" kubeconfig </dev/null >"$E2E_SANDBOX_DIR/kc-files.yaml" || fail "files: kubeconfig"
nodes="$(kubectl --kubeconfig "$E2E_SANDBOX_DIR/kc-files.yaml" get nodes -o name || true)"
case "$nodes" in node/*) ;; *) fail "files: kubeconfig did not print the cached kind kubeconfig" ;; esac
printf 'ok: files: the cached kubeconfig reads with the walk age key and reaches the cluster\n'

phase "the lock, at terminals of the walk's own"
python3 - "$APPRAFTER" "$APPRAFTER_BIN" "$E2E_SANDBOX_DIR" "$APPRAFTER_AGE_KEY" <<'PY'
"""The lock legs at pseudo-terminals of the walk's own (store-lock-walk.sh's header)."""
import fcntl, json, os, select, subprocess, sys, termios, time

A, BIN, SANDBOX, AGE = sys.argv[1:5]
STORE = os.environ["APPRAFTER_CONFIG_DIR"]
PASSWORD = "walk-the-river"          # 14 characters: the advice, kept with `y` (O2)
TOKEN = "w" * 64
BASE = {k: v for k, v in os.environ.items() if k != "APPRAFTER_AGE_KEY"}
BASE.update({"NO_COLOR": "1", "TERM": "dumb", "A": A})
WITH_KEY = {"APPRAFTER_AGE_KEY": AGE}


def fail(what, shown=""):
    print(f"FAILED: {what}\n{shown}", flush=True)
    sys.exit(1)


def ok(what):
    print(f"ok: {what}", flush=True)


class Term:
    """`sh -c script` leading a session of its own, whose controlling terminal is a new pty:
    the commands it runs share that terminal's unlock window (T52)."""

    def __init__(self, script, extra=None):
        self.master, slave = os.openpty()
        self.p = subprocess.Popen(
            ["sh", "-c", script], env=dict(BASE, **(extra or {})), stdin=slave,
            stdout=slave, stderr=slave, start_new_session=True,
            preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
        os.close(slave)
        self.seen = bytearray()
        self.at = 0

    def _read(self, wait):
        ready, _, _ = select.select([self.master], [], [], wait)
        if not ready:
            return None
        try:
            chunk = os.read(self.master, 4096)
        except OSError:
            return False
        if not chunk:
            return False
        self.seen.extend(chunk)
        return True

    def text(self):
        return self.seen.decode(errors="replace")

    def expect(self, want, timeout=120):
        w = want.encode()
        deadline = time.time() + timeout
        while self.seen.find(w, self.at) < 0:
            if time.time() > deadline or self._read(0.1) is False:
                fail(f"{want!r} not shown", self.text())
        self.at = self.seen.find(w, self.at) + len(w)
        return time.time()

    def type(self, s):
        os.write(self.master, (s + "\r").encode())

    def ctrl_c(self):
        os.write(self.master, b"\x03")

    def key_shown(self):
        lines = [l.strip() for l in self.text().splitlines()]
        heads = [i for i, l in enumerate(lines) if l.startswith("APPRAFTER-RECOVERY-KEY-1")]
        if not heads:
            fail("no recovery key on the terminal", self.text())
        i = heads[0]
        rest = [l for l in lines[i + 1:] if l]
        return lines[i] + " " + rest[0]

    def end(self, timeout=120):
        deadline = time.time() + timeout
        while self.p.poll() is None:
            if time.time() > deadline:
                self.p.kill()
                fail("the terminal's shell did not end", self.text())
            self._read(0.1)
        while self._read(0.2) is True:
            pass
        os.close(self.master)
        return self.p.returncode


def plain(args, extra=None):
    """`apprafter args` with no terminal at all: stdin /dev/null, a session of its own."""
    p = subprocess.run([A, *args], env=dict(BASE, **(extra or {})), stdin=subprocess.DEVNULL,
                       capture_output=True, text=True, start_new_session=True, timeout=120)
    return p.returncode, p.stdout, p.stderr


def holders():
    """Holders of this walk's binary: a holder's command line is `<binary> __holder`."""
    r = subprocess.run(["pgrep", "-f", "-x", "--", f"{BIN} __holder"],
                       capture_output=True, text=True)
    return r.stdout.split()


def holders_gone(within):
    deadline = time.time() + within
    while holders() and time.time() < deadline:
        time.sleep(0.2)
    return not holders()


def reaches_cluster(path):
    r = subprocess.run(["kubectl", "--kubeconfig", path, "get", "nodes", "-o", "name"],
                       capture_output=True, text=True, timeout=60)
    return r.returncode == 0 and r.stdout.startswith("node/")


def cached():
    with open(os.path.join(STORE, "state", "walk", ".apprafter", "state.json")) as f:
        return json.load(f)["hetzner_cloud"]["kubeconfig_age"]


def opens_with_walk_key(armored):
    r = subprocess.run(["age", "-d", "-i", AGE], input=armored, capture_output=True, text=True)
    return r.returncode == 0


def files_holding(needle, root):
    found = []
    for top, _, files in os.walk(root):
        for name in files:
            path = os.path.join(top, name)
            with open(path, errors="replace") as f:
                if needle in f.read():
                    found.append(path)
    return found


def leftover_kubeconfigs():
    return [os.path.join(t, n) for t, _, fs in os.walk(SANDBOX) for n in fs
            if n.startswith("kubeconfig-")]


if holders():
    fail("a holder of this binary runs before the walk", " ".join(holders()))

# lock on (O2, O12, T25, T36).
t = Term('"$A" lock on', WITH_KEY)
t.expect("Lock the secrets of the target store at ")
t.expect("New lock password for ")
t.type(PASSWORD)
t.expect("Repeat the lock password: ")
t.type(PASSWORD)
t.expect("This lock password has 14 characters; 15 or more is recommended.")
t.expect("Use it anyway? [y/N] ")
t.type("y")
t.expect("Type the recovery key to confirm you saved it")
key = t.key_shown()
t.type(key[:-1] + ("P" if key[-1] != "P" else "Q"))     # the key is shown upper-case
t.expect("Type it again, or press Ctrl-C to stop (nothing has been changed).")
t.type(key)
t.expect("Locked: the secrets of")
code = t.end()
if code != 0:
    fail(f"lock on: exit {code}", t.text())
for want in ("1 token(s) moved into the vault", "1 cached secret(s) re-encrypted"):
    if want not in t.text():
        fail(f"lock on did not say {want!r}", t.text())
if files_holding(TOKEN, STORE):
    fail("a token in clear after lock on", " ".join(files_holding(TOKEN, STORE)))
with open(os.path.join(STORE, "store.yaml")) as f:
    if "secrets: locked" not in f.read():
        fail("store.yaml does not say `secrets: locked`")
if os.stat(os.path.join(STORE, "vault")).st_mode & 0o777 != 0o600:
    fail("the vault is not owner-only")
if opens_with_walk_key(cached()):
    fail("the cached kubeconfig still opens with the shared age key after lock on")
if not holders_gone(5):
    fail("lock on left a holder", " ".join(holders()))
ok("lock on: the advice under 15 characters, the key typed back whole, nothing in clear, "
   "the cache re-sealed")

# One terminal's window (T52, T56, T66, T45).
kc = os.path.join(SANDBOX, "kc-window.yaml")
kc_nostdin = os.path.join(SANDBOX, "kc-window-nostdin.yaml")
t = Term('"$A" unlock; echo "A:unlock=$?"; '
         f'"$A" kubeconfig > "{kc}"; echo "A:kubeconfig=$?"; '
         f'"$A" kubeconfig < /dev/null > "{kc_nostdin}"; echo "A:nostdin=$?"; '
         '"$A" doctor --no-ping; echo "A:doctor=$?"; '
         '"$A" lock status; echo "A:status=$?"; '
         '"$A" lock now; "$A" lock status; echo "A:end"')
t.expect("Unlock the target store at ")
t.expect("Lock password or recovery key: ")
t.type("not the password")
t.expect("Lock password or recovery key: ")
entered = time.time()
t.type(PASSWORD)
opened = t.expect("Unlocked in this terminal for 5 min")
print(f"timing: unlock opened the vault {opened - entered:.1f} s after Enter (release Argon2id)",
      flush=True)
t.expect("A:unlock=0")
t.expect("which is outside the lock of the target store")
t.expect("A:kubeconfig=0")
t.expect("A:nostdin=0")
t.expect("A:doctor=")
doctor = t.text().split("A:nostdin=0", 1)[1].split("A:doctor=", 1)[0]
for want in ("✓ Kube API reachable", "Secrets locked"):     # rows of different groups: any order
    if want not in doctor:
        fail(f"doctor did not show {want!r}", doctor)
t.expect("This terminal:  unlocked, ")
t.expect("A:status=0")
t.expect("The unlock window in this terminal is closed.")
t.expect("This terminal:  no unlock window")
t.expect("A:end")
code = t.end()
if code != 0:
    fail(f"the window terminal: exit {code}", t.text())
if "Lock password or recovery key (" in t.text():
    fail("a command inside the window asked again", t.text())
for path in (kc, kc_nostdin):
    if not reaches_cluster(path):
        fail(f"{path} does not reach the cluster")
if not holders_gone(5):
    fail("the window's holder outlived `lock now`", " ".join(holders()))
if leftover_kubeconfigs():
    fail("a decrypted kubeconfig was left behind", " ".join(leftover_kubeconfigs()))
ok("window: unlock (a wrong secret asked again), then kubeconfig, doctor's cluster check and "
   "lock status with no other prompt; lock now closes it")

# Another terminal, no terminal, the overrides, target show, the end of a terminal.
a = Term('"$A" unlock; echo A:ready; read line; echo A:bye')
a.expect("Lock password or recovery key: ")
a.type(PASSWORD)
a.expect("A:ready")
if len(holders()) != 1:
    fail("terminal A's window has no single holder", " ".join(holders()))
b = Term('"$A" kubeconfig > /dev/null; echo "B:kubeconfig=$?"')
b.expect("Lock password or recovery key (unlocks this terminal for 5 min): ")
b.ctrl_c()
b.expect("operation cancelled")
b.expect("B:kubeconfig=")
b.end()
if "B:kubeconfig=0" in b.text():
    fail("Ctrl-C at the prompt did not stop kubeconfig", b.text())
ok("another terminal is asked (A's window is A's alone); Ctrl-C at the prompt stops the command")
code, out, err = plain(["kubeconfig"])
if code == 0 or "apprafter::secrets::store_locked" not in err:
    fail("no terminal: not store_locked", err)
ok("no terminal: store_locked")
code, out, err = plain(["target", "ip"], {"HCLOUD_TOKEN": "x" * 64})
if code == 0 or "apprafter::secrets::override_refused" not in err or "HCLOUD_TOKEN" not in err:
    fail("HCLOUD_TOKEN on a locked store: not override_refused", err)
code, out, err = plain(["kubeconfig"], WITH_KEY)
if code == 0 or "apprafter::secrets::override_refused" not in err or "APPRAFTER_AGE_KEY" not in err:
    fail("APPRAFTER_AGE_KEY on a locked store: not override_refused", err)
code, out, err = plain(["target", "list"], {"HCLOUD_TOKEN": "x" * 64})
if code != 0:
    fail("target list needs no token, yet failed with HCLOUD_TOKEN set", err)
ok("overrides: HCLOUD_TOKEN and APPRAFTER_AGE_KEY refused where they would be used; "
   "target list still runs")
code, out, err = plain(["target", "show", "walk"])
if code != 0 or "in the vault" not in out:
    fail("target show does not name the vault", out + err)
ok("target show names the vault and opens nothing")
a.type("")
a.expect("A:bye")
a.end()
if not holders_gone(5):
    fail("terminal A's holder outlived its terminal", " ".join(holders()))
ok("closing terminal A ended its window")

# lock on --timeout on a locked store (O11, T33).
t = Term('"$A" lock on --timeout 15')
t.expect("Lock password or recovery key (")
t.type(key)
t.expect("now last 15 min")
code = t.end()
if code != 0:
    fail(f"lock on --timeout 15: exit {code}", t.text())
if not holders_gone(5):
    fail("a holder outlived the `lock on --timeout` terminal", " ".join(holders()))
ok("lock on --timeout 15 on a locked store asks for a secret and sets 15 min")

# lock off from a second terminal while the first holds a window (e Deviations 31).
a = Term('"$A" unlock; echo A:ready; read line; "$A" lock status; echo A:done')
a.expect("in this terminal for 15 min.")
a.expect("Lock password or recovery key: ")
a.type(PASSWORD)
a.expect("A:ready")
if len(holders()) != 1:
    fail("terminal A's window has no single holder", " ".join(holders()))
b = Term('"$A" lock off', WITH_KEY)
b.expect("Put the secrets of the target store at ")
b.expect("Lock password or recovery key of ")
b.type(PASSWORD)
b.expect("Unlocked for good: the secrets of")
code = b.end()
if code != 0:
    fail(f"lock off: exit {code}", b.text())
if not holders_gone(15):
    fail("terminal A's window outlived lock off by 15 s (its store watch looks every 5 s)",
         " ".join(holders()))
a.type("")
a.expect("in files")
a.expect("A:done")
a.end()
ok("lock off from a second terminal: the first terminal's window ended within its store watch")
with open(os.path.join(STORE, "targets", "walk", "credentials.yaml")) as f:
    if TOKEN not in f.read():
        fail("the token is not back in its credentials file")
if os.path.exists(os.path.join(STORE, "vault")):
    fail("the vault is still there after lock off")
if not opens_with_walk_key(cached()):
    fail("the cached kubeconfig does not open with the walk age key after lock off")
after = os.path.join(SANDBOX, "kc-after.yaml")
code, out, err = plain(["kubeconfig"], WITH_KEY)
with open(after, "w") as f:
    f.write(out)
if code != 0 or not reaches_cluster(after):
    fail("kubeconfig after lock off", err)
ok("lock off: the token and the cache back in files, the cache under the walk age key")
if holders():
    fail("a holder outlived the walk", " ".join(holders()))
if leftover_kubeconfigs():
    fail("a decrypted kubeconfig was left behind", " ".join(leftover_kubeconfigs()))
print("WALK GREEN", flush=True)
PY
