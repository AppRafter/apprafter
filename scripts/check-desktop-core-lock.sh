#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-desktop-core-lock.sh — apprafter-core must compile against the same crate versions
# in cli/ and in desktop/ (design spec §3.4, ADR 0067).
#
# cli/ and desktop/ are separate Cargo workspaces with separate locks, and the desktop links
# the core by path. If the two locks resolve a crate of the core's closure differently, the
# core the desktop ships is not the core the CLI tested.
#
# Rule: walk the dependency closure of the path package `apprafter-core` (normal + build
# edges, every platform) in each workspace and collect {name: {versions}}. FAIL when a name
# present in BOTH closures has a different version set. REPORT, but do not fail, names
# present in only one closure: each workspace unifies features across its own members, so an
# optional dependency Tauri switches on legitimately appears on one side only.
#
# When the desktop needs a newer shared crate, cli/Cargo.lock moves first, in a CLI release.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

if [ ! -f desktop/Cargo.toml ]; then
    echo "==> no desktop/Cargo.toml — nothing to compare"
    exit 0
fi

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
( cd cli && cargo metadata --format-version 1 --locked --all-features ) >"$tmp/cli.json"
( cd desktop && cargo metadata --format-version 1 --locked --all-features ) >"$tmp/desktop.json"

python3 - "$tmp/cli.json" "$tmp/desktop.json" <<'PYEOF'
import json, sys

def closure(path):
    meta = json.load(open(path))
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [i for i, p in pkgs.items() if p["name"] == "apprafter-core" and p["source"] is None]
    if len(roots) != 1:
        sys.exit(f"ERROR: {path}: expected one path package apprafter-core, found {len(roots)}")
    seen, todo = set(), [roots[0]]
    while todo:
        cur = todo.pop()
        if cur in seen:
            continue
        seen.add(cur)
        for dep in nodes[cur]["deps"]:
            if any(k["kind"] in (None, "build") for k in dep["dep_kinds"]):
                todo.append(dep["pkg"])
    out = {}
    for i in seen:
        p = pkgs[i]
        if p["source"] is None:
            continue  # path crates (the core itself, cli-core, …) are the same files on both sides
        out.setdefault(p["name"], set()).add(p["version"])
    return out

cli, desk = closure(sys.argv[1]), closure(sys.argv[2])
shared = sorted(set(cli) & set(desk))
bad = [(n, sorted(cli[n]), sorted(desk[n])) for n in shared if cli[n] != desk[n]]
only_cli = sorted(set(cli) - set(desk))
only_desk = sorted(set(desk) - set(cli))
print(f"==> apprafter-core closure: {len(cli)} crates in cli/, {len(desk)} in desktop/, {len(shared)} shared")
if only_cli:
    print(f"    only in cli/ (feature unification, not an error): {', '.join(only_cli)}")
if only_desk:
    print(f"    only in desktop/ (feature unification, not an error): {', '.join(only_desk)}")
if bad:
    print("ERROR: apprafter-core resolves to different versions in cli/ and desktop/:", file=sys.stderr)
    for n, a, b in bad:
        print(f"  {n}: cli/ {', '.join(a)}  vs  desktop/ {', '.join(b)}", file=sys.stderr)
    print("Fix: align desktop/Cargo.lock to cli/Cargo.lock (cargo update -p <crate> --precise <cli version>"
          " in desktop/); if the desktop needs the newer version, move cli/Cargo.lock first, in a CLI release.",
          file=sys.stderr)
    sys.exit(1)
print("OK: every shared crate of the apprafter-core closure has the same versions in cli/ and desktop/.")
PYEOF
