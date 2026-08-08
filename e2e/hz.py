# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""Hetzner Cloud project sweep/verify helper for real-infra e2e walks.
Baseline for the test projects is EMPTY, so `sweep` safely deletes every
resource of every type and `verify` asserts the project is back to zero.
Never prints the token. Usage: hz.py {sweep|verify|list} <token>
"""
import json
import sys
import time
import urllib.error
import urllib.request

API = "https://api.hetzner.cloud/v1"
KINDS = [
    "servers", "load_balancers", "volumes", "floating_ips",
    "primary_ips", "firewalls", "networks", "ssh_keys",
]


def req(token, method, path):
    r = urllib.request.Request(API + path, method=method,
                               headers={"Authorization": "Bearer " + token})
    try:
        with urllib.request.urlopen(r, timeout=30) as resp:
            body = resp.read()
            return resp.status, (json.loads(body) if body else {})
    except urllib.error.HTTPError as e:
        return e.code, {"error": e.read().decode("utf-8", "replace")[:200]}
    except Exception as e:  # noqa: BLE001
        return 0, {"error": str(e)}


def listing(token, kind):
    _, d = req(token, "GET", "/%s?per_page=50" % kind)
    return d.get(kind, []) if isinstance(d, dict) else []


def counts(token):
    return {k: len(listing(token, k)) for k in KINDS}


def cmd_list(token):
    for k in KINDS:
        items = listing(token, k)
        names = ", ".join("%s#%s(%s)" % (i.get("name", "?"), i.get("id"),
                          i.get("status", "")) for i in items) or "(none)"
        print("  %-14s %d : %s" % (k, len(items), names))


def cmd_verify(token):
    c = counts(token)
    total = sum(c.values())
    print("  " + "  ".join("%s=%d" % (k, v) for k, v in c.items()))
    if total == 0:
        print("  VERIFY OK — project empty (0 resources)")
        return 0
    print("  VERIFY FAIL — %d resource(s) remain" % total)
    return 1


def cmd_sweep(token):
    for attempt in range(1, 4):
        for k in KINDS:
            for it in listing(token, k):
                st, resp = req(token, "DELETE", "/%s/%d" % (k, it["id"]))
                ok = st in (200, 202, 204)
                print("  del %-13s #%s -> HTTP %s %s"
                      % (k, it["id"], st, "" if ok else resp.get("error", "")))
        time.sleep(6)
        left = sum(counts(token).values())
        print("  pass %d: %d resource(s) left" % (attempt, left))
        if left == 0:
            break
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(2)
    fn = {"sweep": cmd_sweep, "verify": cmd_verify, "list": cmd_list}.get(sys.argv[1])
    if not fn:
        print("unknown action", sys.argv[1])
        sys.exit(2)
    sys.exit(fn(sys.argv[2]) or 0)
