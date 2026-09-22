#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# Lint the GitHub Actions workflows: action pins first, then actionlint.
#
#     scripts/check-workflows.sh            # offline; what pre-commit runs
#     scripts/check-workflows.sh --remote   # also prove each pin against upstream
#
# THE DEFECT CLASS THIS EXISTS FOR. A workflow file that does not parse
# does not fail a check — it fails as a RUN WITH ZERO JOBS, which
# `gh pr checks` does not list at all. Every PR check can be green while
# a release workflow is broken, and nothing says so until the day it was
# supposed to run. That is how `release-cli.yml` broke on 2026-09-22: a
# shell COMMENT inside a `run:` block contained empty Actions expression
# delimiters. Actions expands those before the shell sees the script, so
# it does not know the line is a comment; an empty expression is
# unparseable and took the whole file down.
#
# actionlint catches that class and a good deal more (unknown `needs:`
# targets, bad `runs-on`, misspelled contexts) without a network call or
# a container.
#
# ACTION PINS — THE CONVENTION. Every `uses:` names its action by the full
# 40-hex COMMIT SHA, with the release that commit is as a trailing comment:
#
#     uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
#
# A tag — even a full vX.Y.Z — is a mutable pointer. Its owner, or anyone
# holding a stolen token of theirs, can re-point it, and every workflow
# naming it runs the new code on its next run with nothing in this
# repository changing. That is how the tj-actions/changed-files compromise
# (March 2025) reached thousands of repositories: existing version tags were
# re-pointed at a commit that dumped CI secrets into the build logs. A commit
# SHA is the only immutable ref. It is the same reasoning that keeps
# kindest/node digest-pinned in e2e/lib.sh, and it applies to first-party
# `actions/*` too — a stolen token is not only a third-party risk, and one
# rule with no exceptions is the rule a reviewer can hold.
#
# The price: a SHA freezes the patch stream as well, so a security fix in an
# action arrives only when someone bumps it. scripts/upstream-versions.py
# reads the version COMMENT and compares it numerically, so every newer
# release — patch included — shows up as BEHIND in the weekly report.
#
# To bump: resolve the release's COMMIT, never the tag object —
#
#     git ls-remote --tags https://github.com/<owner>/<repo> 'refs/tags/vX.Y.Z*'
#
# prints `<sha> refs/tags/vX.Y.Z` and, for an annotated tag, a second line
# `<sha> refs/tags/vX.Y.Z^{}`. Only the `^{}` line is the commit; the
# first is the tag object. Replace the SHA and the comment together in
# EVERY workflow, then run this script with `--remote`.
#
# What the offline pass enforces (a comment is what humans and the watcher
# read, so it is held to one shape and kept honest):
#   1. every `uses:` is `owner/repo[/path]@<40 lowercase hex> # v<N[.N[.N]]>`;
#      a local `./` action is exempt (it moves with this commit), and a
#      `docker://` image must be digest-pinned (`@sha256:<64 hex>`);
#   2. one SHA per action across all workflows — two SHAs is a half-applied
#      bump, and one of the two comments is then stale;
#   3. one version comment per SHA — one commit cannot be two releases; a
#      mismatch means a SHA moved without its comment or the reverse;
#   4. every dtolnay/rust-toolchain step passes `toolchain:`. At a commit
#      (as opposed to its `@stable`-style branches) the input is required,
#      and GitHub does not enforce `required:` — the step fails at run time,
#      which for release-cli.yml is the day of a release.
# What it cannot do offline is prove that the comment names the release
# the SHA really is. `--remote` does that with `git ls-remote`, and fails on
# a tag that does not exist or points elsewhere (the comment is wrong, or
# upstream re-pointed the tag — both need a human). It needs the network,
# so the pre-commit hook does not pass it; run it after every bump.
#
# CHECK_WORKFLOWS_DIR=<dir> points both passes at the *.yml/*.yaml in <dir>
# instead of the tracked workflows — how the pin rules are mutation-tested
# (copy the workflows to a scratch dir, break one, watch this fail).
#
# Override the actionlint binary by exporting ACTIONLINT=<path>. With
# neither actionlint nor nix available the script exits non-zero and says
# so, rather than passing silently — a gate that skips when its tool is
# missing is not a gate.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

REMOTE=0
for arg in "$@"; do
    case "$arg" in
        --remote) REMOTE=1 ;;
        -h | --help)
            sed -n '4,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $arg (expected --remote)" >&2
            exit 2
            ;;
    esac
done

# The workflow set is passed in rather than globbed inside the checker so a
# test can point it at scratch copies. `git ls-files` for the real run: an
# untracked scratch workflow is not part of the tree CI will execute.
WORKFLOWS_DIR="${CHECK_WORKFLOWS_DIR:-.github/workflows}"
if [[ -n "${CHECK_WORKFLOWS_DIR:-}" ]]; then
    mapfile -t workflow_files < <(find "$WORKFLOWS_DIR" -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) | sort)
else
    mapfile -t workflow_files < <(git ls-files -- "$WORKFLOWS_DIR/*.yml" "$WORKFLOWS_DIR/*.yaml")
fi
if [[ ${#workflow_files[@]} -eq 0 ]]; then
    # An empty input must never read as "all pins fine".
    echo "ERROR: no workflow files found under $WORKFLOWS_DIR" >&2
    exit 2
fi

echo "==> action pins (${#workflow_files[@]} workflows)"
pins_rc=0
REMOTE="$REMOTE" python3 - "${workflow_files[@]}" <<'PY' || pins_rc=$?
import os
import re
import subprocess
import sys
from collections import defaultdict

files = sys.argv[1:]
remote = os.environ.get("REMOTE") == "1"

# A `uses:` KEY: first token of the line, optionally after a list dash.
# Matching on the key keeps prose and `run:` text that merely mentions
# "uses:" out of scope.
USES = re.compile(r"^(?P<ind>[ ]*)(?P<dash>-[ ]+)?uses:[ \t]*(?P<val>.*?)[ \t]*$")
REF = re.compile(r"^(?P<q>['\"]?)(?P<ref>[^'\"\s#]+)(?P=q)(?P<rest>.*)$")
PINNED = re.compile(
    r"^(?P<repo>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)(?:/[^@\s]+)?@(?P<sha>[0-9a-f]{40})$"
)
COMMENT = re.compile(r"^[ \t]+#[ \t]+(?P<ver>v[0-9]+(?:\.[0-9]+){0,2})[ \t]*$")
DOCKER = re.compile(r"^docker://[^@\s]+@sha256:[0-9a-f]{64}$")

errors = []
sha_by_repo = defaultdict(set)      # repo(lower) -> {sha}
ver_by_sha = defaultdict(set)       # sha -> {version}
where = defaultdict(list)           # (repo, sha) -> [file:line]
display = {}                        # repo(lower) -> spelling as written
count = 0

for path in files:
    with open(path, encoding="utf-8") as fh:
        lines = fh.read().split("\n")
    for idx, line in enumerate(lines):
        m = USES.match(line)
        if not m:
            continue
        count += 1
        loc = f"{path}:{idx + 1}"
        r = REF.match(m["val"])
        if not r:
            errors.append(f"{loc}: cannot parse the `uses:` value: {m['val']!r}")
            continue
        ref, rest = r["ref"], r["rest"]
        if ref.startswith("./"):
            continue
        if ref.startswith("docker://"):
            if not DOCKER.match(ref):
                errors.append(f"{loc}: `{ref}` -- a docker:// action must be pinned by @sha256:<digest>")
            continue
        p = PINNED.match(ref)
        c = COMMENT.match(rest)
        if not p or not c:
            want = "owner/repo@<40-hex commit SHA> # v<version>"
            errors.append(f"{loc}: `{m['val']}` -- expected `{want}`")
            continue
        repo = p["repo"].lower()
        display.setdefault(repo, p["repo"])
        sha_by_repo[repo].add(p["sha"])
        ver_by_sha[p["sha"]].add(c["ver"])
        where[(repo, p["sha"])].append(loc)

        # Rule 4: the step must pass `toolchain:`. The step is every line
        # indented deeper than its list dash, up to the next shallower line.
        if repo == "dtolnay/rust-toolchain":
            key_col = len(m["ind"]) + len(m["dash"] or "")
            dash_col = key_col - 2
            start = idx
            if not m["dash"]:
                while start > 0 and not re.match(rf"^ {{{dash_col}}}- ", lines[start]):
                    start -= 1
            end = idx + 1
            while end < len(lines):
                nxt = lines[end]
                if nxt.strip() and (len(nxt) - len(nxt.lstrip(" "))) < key_col:
                    break
                end += 1
            step = lines[start:end]
            if not any(re.match(rf"^ {{{key_col + 1},}}toolchain:[ \t]*\S", s) for s in step):
                errors.append(
                    f"{loc}: dtolnay/rust-toolchain at a commit SHA has no default toolchain -- "
                    "pass `with: toolchain:` (the minor, quoted, e.g. \"1.98\")"
                )

if count == 0:
    errors.append("no `uses:` keys found at all -- the pattern no longer matches this tree")

for repo, shas in sorted(sha_by_repo.items()):
    if len(shas) > 1:
        def at(sha, cap=3):
            locs = where[(repo, sha)]
            more = f" +{len(locs) - cap} more" if len(locs) > cap else ""
            return ", ".join(locs[:cap]) + more
        detail = "; ".join(
            f"{sha[:12]} {'/'.join(sorted(ver_by_sha[sha]))} at {at(sha)}" for sha in sorted(shas)
        )
        errors.append(f"{display[repo]}: {len(shas)} different SHAs across workflows -- one action, one SHA ({detail})")

for sha, vers in sorted(ver_by_sha.items()):
    if len(vers) > 1:
        errors.append(f"SHA {sha}: {len(vers)} different version comments ({', '.join(sorted(vers))}) -- one commit, one release")

if remote and not errors:
    for repo, shas in sorted(sha_by_repo.items()):
        sha = next(iter(shas))
        for ver in sorted(ver_by_sha[sha]):
            url = f"https://github.com/{display[repo]}"
            proc = subprocess.run(
                ["git", "ls-remote", "--tags", url, f"refs/tags/{ver}", f"refs/tags/{ver}^{{}}"],
                capture_output=True, text=True,
            )
            if proc.returncode != 0:
                errors.append(f"{display[repo]}: git ls-remote failed: {proc.stderr.strip()[:200]}")
                continue
            refs = dict(reversed(l.split("\t", 1)) for l in proc.stdout.splitlines() if "\t" in l)
            commit = refs.get(f"refs/tags/{ver}^{{}}") or refs.get(f"refs/tags/{ver}")
            if commit is None:
                errors.append(f"{display[repo]}: tag {ver} does not exist upstream -- the comment is wrong")
            elif commit != sha:
                errors.append(
                    f"{display[repo]}: tag {ver} is commit {commit} upstream, the workflows pin {sha} -- "
                    "the comment is wrong, or upstream re-pointed the tag"
                )
            else:
                print(f"    {display[repo]} {ver} = {sha[:12]} (verified upstream)")

if errors:
    print(f"ERROR: {len(errors)} action-pin problem(s):", file=sys.stderr)
    for e in errors:
        print(f"  {e}", file=sys.stderr)
    sys.exit(1)
print(f"OK: {count} `uses:` across {len(files)} workflows; {len(sha_by_repo)} actions, each on one commit SHA"
      + (", every comment verified upstream" if remote else ""))
PY

if [[ -n "${ACTIONLINT:-}" ]]; then
    ACTIONLINT_CMD=("$ACTIONLINT")
elif command -v actionlint >/dev/null 2>&1; then
    ACTIONLINT_CMD=(actionlint)
elif command -v nix >/dev/null 2>&1; then
    ACTIONLINT_CMD=(nix run nixpkgs#actionlint --)
else
    echo "ERROR: actionlint is not installed and nix is unavailable." >&2
    echo "Install it from https://github.com/rhysd/actionlint or run" >&2
    echo "'nix develop' first." >&2
    exit 2
fi

# Two shellcheck STYLE codes predate this gate and are not what it is for:
#   SC2129 — prefer `{ a; b; } >> file` over repeated redirects
#   SC2006 — prefer $(...) over legacy backticks
# They appear in argocd-cue-cmp-publish.yml and platform-stack-publish.yml.
# Ignoring them keeps the gate's output actionable instead of a wall a
# reader learns to skip; anything that can actually break a workflow —
# syntax, expressions, contexts, job graph — still fails here. Fixing the
# two is worth its own change, not a drive-by inside a release.
echo "==> actionlint $WORKFLOWS_DIR/"
lint_rc=0
"${ACTIONLINT_CMD[@]}" \
    -ignore 'shellcheck reported issue in this script: SC2129' \
    -ignore 'shellcheck reported issue in this script: SC2006' \
    "${workflow_files[@]}" || lint_rc=$?

if [[ $pins_rc -ne 0 || $lint_rc -ne 0 ]]; then
    echo "FAILED: action pins exit $pins_rc, actionlint exit $lint_rc." >&2
    exit 1
fi
echo "OK: workflows parse, lint clean, and every action is SHA-pinned."
