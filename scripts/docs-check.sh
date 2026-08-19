#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Local-first documentation gate, in four passes:
#
#   1. the strict mkdocs build (dead links, dead anchors, unlisted
#      pages, missing snippet includes);
#   2. the LLM artefacts the build hook writes — llms.txt,
#      llms-full.txt and one markdown twin per page — checked against
#      the site that was just built;
#   3. `docsgen check` — the byte-compare of the GENERATED CLI
#      reference under docs/reference/cli/ against the clap tree;
#   4. `docsgen gate` — every claim in the HAND-WRITTEN documentation
#      resolved against the product: `apprafter` invocations against
#      the clap tree, schema paths against the shipped CRDs, complete
#      CUE manifests through one batched `cue vet`.
#
# Pass 2 exists because the hook that writes those three artefacts is
# invisible when it breaks: mkdocs does not know they were supposed to
# exist, so a hook that stopped writing them — or wrote an index that
# has quietly lost a nav group — leaves a green build and a published
# site with a broken machine-readable layer. Every expectation it
# asserts is derived from the site that was just built, so it stays
# true as pages are added: nothing here restates a page list.
#
# Order matters throughout: the mkdocs build reports on what is
# committed, so it runs first and its failures (a dead link a
# contributor wrote) are not hidden behind a regeneration reminder;
# and `check` precedes `gate` because a stale generated reference is
# a one-command fix that would otherwise be buried under a work-list.
#
# `gate` exits 1 for findings and 2 when the gate itself broke (no
# `cue`, an unreadable page). `set -e` fails the script on either —
# the codes are for a reader of the log, not for control flow here.
#
# Runs under `nix develop` so mkdocs, the theme and the plugins are
# the flake.lock-pinned versions: a byte-compare needs ONE toolchain
# across local + CI. Builds into a scratch dir so ./site is never
# clobbered by a check.
set -euo pipefail
if ! command -v nix >/dev/null 2>&1; then
    echo "ERROR: docs-check needs nix — mkdocs, the material theme and the" >&2
    echo "plugins must be the flake.lock-pinned versions, because a later" >&2
    echo "change byte-compares generated pages against them. There is no" >&2
    echo "cue-style fall back to a system binary here." >&2
    echo >&2
    echo "Install nix (flakes enabled) — see docs/contributing/setup.md — or" >&2
    echo "skip the local hook with 'git commit --no-verify'; the docs workflow" >&2
    echo "still runs on the pull request." >&2
    exit 2
fi
cd "$(dirname "$0")/.."
repo_root="$PWD"
export repo_root
exec nix develop --command bash -c '
  set -euo pipefail
  out="$(mktemp -d)"
  trap '"'"'rm -rf "$out"'"'"' EXIT
  mkdocs build --strict --site-dir "$out/site"
  python3 - "$out/site" <<"PY"
import re
import sys
from pathlib import Path

from mkdocs.config import load_config

site = Path(sys.argv[1])
base = load_config("mkdocs.yml")["site_url"]
problems = []

# What the site actually published, as site-root-relative URL paths.
# Deriving the expectation from the build output is the whole point:
# a page added tomorrow is covered without editing this script.
published = sorted(
    "" if p.parent == site else p.parent.relative_to(site).as_posix() + "/"
    for p in site.rglob("index.html")
    if not p.relative_to(site).as_posix().startswith("assets/")
)
# The ADRs are bundled and twinned but deliberately not indexed --
# docs/hooks/llm_export.py says why.
indexable = [u for u in published if not u.startswith("adr/")]


def show(paths):
    # The home page is the empty path; printed bare it reads as a stray
    # comma in a list of five.
    return ", ".join(p or "<site root>" for p in sorted(paths)[:5])


index_path = site / "llms.txt"
index = ""
if not index_path.is_file():
    problems.append("llms.txt was not written")
else:
    index = index_path.read_text()
    if "CC-BY-4.0" not in index:
        problems.append("llms.txt carries no licence line (nothing in it names CC-BY-4.0)")

urls = re.findall(r"(?m)^- \[[^\]]*\]\((\S+)\)", index)
relative = [u for u in urls if not u.startswith(base)]
if relative:
    problems.append("llms.txt links are not absolute: " + show(relative))
listed = {u[len(base):] for u in urls if u.startswith(base)}
dangling = sorted(u for u in listed if u not in set(published))
if dangling:
    problems.append("llms.txt links pages the site does not contain: " + show(dangling))
unlisted = [u for u in indexable if u not in listed]
if unlisted:
    problems.append(
        str(len(unlisted))
        + " published page(s) absent from llms.txt, so at least one nav group is"
        + " short or missing: "
        + show(unlisted)
    )

full_path = site / "llms-full.txt"
if not full_path.is_file():
    problems.append("llms-full.txt was not written")
else:
    bundled = re.findall(
        # `audience:` is [^\n]+ rather than \S+ so an author writing a
        # two-word audience does not read here as a missing page.
        r"(?ms)^---\nurl: (\S+)\n(?:description: [^\n]*\n)?audience: [^\n]+\n---\n(.*?)(?=^---\nurl: |\Z)",
        full_path.read_text(),
    )
    if len(bundled) != len(published):
        problems.append(
            "llms-full.txt bundles " + str(len(bundled)) + " page(s); the site published "
            + str(len(published))
        )
    hollow = [u for u, body in bundled if not body.strip()]
    if hollow:
        problems.append(
            str(len(hollow)) + " llms-full.txt entr(ies) are a header with no page body"
            + " -- the bundle is no bigger than its headers: " + show(hollow)
        )

named = site / "dev-guide" / "quickstart.md"
if not named.is_file():
    problems.append("no markdown twin at " + named.relative_to(site).as_posix())
twins = list(site.rglob("*.md"))
if len(twins) != len(published):
    problems.append(
        "the site holds " + str(len(twins)) + " markdown twin(s) for "
        + str(len(published)) + " published page(s)"
    )

if problems:
    sys.stderr.write("ERROR: docs/hooks/llm_export.py did not deliver:\n")
    for problem in problems:
        sys.stderr.write("  - " + problem + "\n")
    sys.stderr.write(
        "The mkdocs build stays GREEN whatever this hook does, so this check is"
        " the only detector. Confirm the hook is still listed under `hooks:` in"
        " mkdocs.yml, then read its module docstring.\n"
    )
    raise SystemExit(1)

print(
    "llm artefacts OK: " + str(len(listed)) + " page(s) indexed in llms.txt, "
    + str(len(published)) + " bundled in llms-full.txt, " + str(len(twins)) + " markdown twin(s)"
)
PY
  cd cli && cargo run --quiet -p docsgen -- check
  cd "$repo_root/cli" && cargo run --quiet -p docsgen -- gate
'
