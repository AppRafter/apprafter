#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# check-desktop-core-lock.sh — the path crates the desktop links from cli/ (apprafter-core,
# and apprafter-os-slots from D.4b) must compile against the same crate versions in cli/ and in
# desktop/ (design spec §3.4, ADR 0067; D.4b B9).
#
# cli/ and desktop/ are separate Cargo workspaces with separate locks, and the desktop links
# these crates by path. If the two locks resolve a crate of their closure differently, the
# crate the desktop ships is not the crate the CLI tested.
#
# Rule: for each root, walk the dependency closure of the path package (normal + build edges,
# every platform) in each workspace and collect {name: {versions}}. FAIL when a name present in
# BOTH closures has a different version set. REPORT, but do not fail, names present in only one
# closure: each workspace unifies features across its own members, so an optional dependency
# Tauri switches on legitimately appears on one side only. A root the desktop does not link yet
# (apprafter-os-slots until d4b-g1b) is held to more: every crate of its cli/ closure must
# already be in desktop/Cargo.lock at cli/'s versions, so linking it adds no package there.
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

ROOTS = ["apprafter-core", "apprafter-os-slots"]

def load(path):
    meta = json.load(open(path))
    return {p["id"]: p for p in meta["packages"]}, {n["id"]: n for n in meta["resolve"]["nodes"]}

def closure(where, pkgs, nodes, root):
    roots = [i for i, p in pkgs.items() if p["name"] == root and p["source"] is None]
    if len(roots) > 1:
        sys.exit(f"ERROR: {where}: more than one path package {root}")
    if not roots:
        return None
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
            continue  # path crates (the root itself, cli-core, …) are the same files on both sides
        out.setdefault(p["name"], set()).add(p["version"])
    return out

cli_pkgs, cli_nodes = load(sys.argv[1])
desk_pkgs, desk_nodes = load(sys.argv[2])
desk_all = {}
for p in desk_pkgs.values():
    if p["source"] is not None:
        desk_all.setdefault(p["name"], set()).add(p["version"])
failed = False
for root in ROOTS:
    cli = closure("cli/", cli_pkgs, cli_nodes, root)
    if cli is None:
        sys.exit(f"ERROR: cli/: no path package {root}")
    desk = closure("desktop/", desk_pkgs, desk_nodes, root)
    if desk is None:
        if root == "apprafter-core":
            sys.exit("ERROR: desktop/: expected one path package apprafter-core, found none")
        missing = [(n, sorted(v), sorted(desk_all.get(n, set())))
                   for n, v in sorted(cli.items()) if not v <= desk_all.get(n, set())]
        print(f"==> {root} closure: {len(cli)} crates in cli/; not linked by desktop/ yet")
        if missing:
            print(f"ERROR: linking {root} would add or move packages in desktop/Cargo.lock:", file=sys.stderr)
            for n, a, b in missing:
                print(f"  {n}: cli/ {', '.join(a)}  vs  desktop/ {', '.join(b) or '(absent)'}", file=sys.stderr)
            failed = True
        else:
            print(f"OK: every crate of the {root} closure is in desktop/Cargo.lock at cli/'s versions.")
        continue
    shared = sorted(set(cli) & set(desk))
    bad = [(n, sorted(cli[n]), sorted(desk[n])) for n in shared if cli[n] != desk[n]]
    only_cli = sorted(set(cli) - set(desk))
    only_desk = sorted(set(desk) - set(cli))
    print(f"==> {root} closure: {len(cli)} crates in cli/, {len(desk)} in desktop/, {len(shared)} shared")
    if only_cli:
        print(f"    only in cli/ (feature unification, not an error): {', '.join(only_cli)}")
    if only_desk:
        print(f"    only in desktop/ (feature unification, not an error): {', '.join(only_desk)}")
    if bad:
        print(f"ERROR: {root} resolves to different versions in cli/ and desktop/:", file=sys.stderr)
        for n, a, b in bad:
            print(f"  {n}: cli/ {', '.join(a)}  vs  desktop/ {', '.join(b)}", file=sys.stderr)
        print("Fix: align desktop/Cargo.lock to cli/Cargo.lock (cargo update -p <crate> --precise <cli version>"
              " in desktop/); if the desktop needs the newer version, move cli/Cargo.lock first, in a CLI release.",
              file=sys.stderr)
        failed = True
    else:
        print(f"OK: every shared crate of the {root} closure has the same versions in cli/ and desktop/.")
if failed:
    sys.exit(1)
PYEOF
