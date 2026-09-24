#!/usr/bin/env python3
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""Upstream version watch: read every hand-written external pin, ask upstream,
and (stage 2) try the candidate against a gate that can actually fail.

Run from the repository root:

    python3 scripts/upstream-versions.py                  # report + gates
    python3 scripts/upstream-versions.py --no-gates       # report only, fast
    python3 scripts/upstream-versions.py --read-only      # tree only, no network
    python3 scripts/upstream-versions.py --only tool-cue  # one entry
    python3 scripts/upstream-versions.py -o report.md     # write instead of stdout

The inventory is `scripts/upstream-pins.json` -- DATA. This file must never
grow a branch per pin; a new pin is an entry there, not code here. The only
dispatch tables are over *kinds* (how to read a value, which API to ask, which
gate to run), and each of those is a handful of lines.

WHY WE OWN THIS
---------------
plan.md 2.23g records the owner's decision to write this rather than adopt
Renovate or Dependabot. The reasoning, in one line: a bot natively reads four
of this repo's eight pin syntaxes, and the other four (CUE chart pins, flake
inputs, workflow `with: version:` inputs, `curl .../releases/download/`
literals) need regexes we write and maintain either way -- so the bot buys the
same volume of code, and additionally takes over the commit format, the
schedule and the PR model. What it cannot buy at any price is the second stage
below.

STAGE 2 -- WHY A SCRIPT BEATS A BOT
-----------------------------------
A bot opens a pull request and hopes a human reads CI. This script applies the
candidate version and runs a gate, so the report distinguishes "newer exists,
gate green" from "newer exists, gate red" -- the second being the actual cost
of the upgrade, which nobody in this repository knows today.

Gates are declared per entry in the inventory, together with a `proves` string
that is PRINTED BESIDE EVERY GREEN. That is deliberate: a `helm template` of a
candidate chart proves the chart accepts our values, and proves nothing about
whether the new controller works in a cluster. An unqualified green would be a
lie, so the format makes an unqualified green impossible to emit.

Entries with no cluster-free gate carry `gateGap` instead of `gate`, and the
report ends with the full list of them. "We do not test this class, and here is
why" is a finding; a silent omission is not.

NO SILENT PASSES
----------------
This is the load-bearing rule (CLAUDE.md; the owner's standing mandate). Four
things are errors, reported in their own section at the TOP of the report and
exiting non-zero:

  * UNREADABLE   a pin's regex matched nothing, or its file glob matched no
                 tracked file. A watcher whose input has quietly become empty
                 reports "all clear" forever; that is the failure mode this
                 status exists to make impossible.
  * TOOL-MISSING a `command` reader's binary is not on PATH. Not a skip.
  * UNREACHABLE  the upstream query failed (network, 404, rate limit, or an
                 index we could not parse). Distinct from "up to date".
  * DRIFT        one logical pin, more than one value in the tree. Not an
                 error against upstream, but it is always a bug, so it gets a
                 section of its own rather than being flattened into a row.

Exit status is 0 when every entry resolved -- INCLUDING when many are behind
and including when a gate went red. An available upgrade is the output, not a
failure. Exit 1 means this script could not do its job.

COMPARISON
----------
`numeric` compares the digit-runs of two strings as a tuple: v1.16.2 ->
(1,16,2), 1.88-alpine3.21 -> (1,88,3,21). That is deliberately cruder than
semver and it handles the tag shapes in this tree, which semver does not.
Candidates whose tag looks like a prerelease are dropped. `major` reports only
when the leading number grows, for refs like `@v4` that float within a major on
purpose. `minor` reports only when (major, minor) grows, for pins written as a
minor (`1.98`, `24.12`) whose patch floats on purpose -- the owner's "pin the
minor, let the patch float" rule; under `numeric` every patch release would
read as BEHIND against a value that deliberately carries no patch. `none` never
reports "behind" and requires a note saying why.

An upstream field may contain `{major}`, replaced by the leading number of the
in-tree value before upstream is asked -- "the newest 24.x", for a pin that
tracks minors within a major it chose deliberately. Deriving it from the tree
rather than writing `24` into the inventory is what keeps the entry honest the
day the tree moves to 26: a hard-coded 24 would then report the tree as ahead
of upstream, i.e. CURRENT, forever.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request
from datetime import datetime, timezone, UTC
from pathlib import Path

INVENTORY = "scripts/upstream-pins.json"
USER_AGENT = "apprafter-upstream-version-watch (+https://github.com/apprafter)"
HTTP_TIMEOUT = 45

# Tag shapes that are never a candidate. Anchored on a separator so that
# "1.88-alpine3.21" (contains neither) and "bookworm-slim" survive, while
# "v1.7.0-rc.1" and "2.0.0-beta3" are dropped.
PRERELEASE = re.compile(
    r"(?i)(^|[-._+])(alpha|beta|rc|dev|preview|pre|snapshot|nightly)([-._]|\d|$)"
)

# Statuses. The first four are "this script could not do its job".
UNREADABLE = "UNREADABLE"
TOOL_MISSING = "TOOL-MISSING"
UNREACHABLE = "UNREACHABLE"
BEHIND = "BEHIND"
CURRENT = "CURRENT"
# On the newest published version and STILL a dead end -- an upstream blocker,
# an abandoned project, a fork that has not landed. "Are we behind?" cannot see
# this class at all, which is how a dead dependency reads healthier in this
# report than a maintained one that ships often.
HELD = "HELD"
FLOATING = "FLOATING"
AGGREGATE = "AGGREGATE"
ERROR_STATUSES = {UNREADABLE, TOOL_MISSING, UNREACHABLE}
COMPARES = {"numeric", "major", "minor", "none"}


class Failure(Exception):
    """A per-entry failure carrying the message the report will print."""


# --------------------------------------------------------------------------
# HTTP
# --------------------------------------------------------------------------


def http_get(url: str, headers: dict[str, str] | None = None) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, **(headers or {})})
    try:
        # urllib follows 3xx for GET, which matters: several Helm repos 301 to
        # another host (cloudnative-pg.github.io -> cloudnative-pg.io) and
        # without the redirect the body is an nginx error page that parses to
        # zero versions -- a false "up to date" of exactly the kind this
        # script exists to prevent.
        with urllib.request.urlopen(req, timeout=HTTP_TIMEOUT) as resp:
            return resp.read()
    except urllib.error.HTTPError as exc:
        detail = ""
        if exc.code in (403, 429) and exc.headers.get("X-RateLimit-Remaining") == "0":
            raw = exc.headers.get("X-RateLimit-Reset")
            try:
                when = datetime.fromtimestamp(int(raw), tz=UTC).strftime("%H:%M UTC")
            except (TypeError, ValueError):
                when = "an unknown time"
            detail = (
                f" -- GitHub API rate limit exhausted (resets at {when}). "
                "Set GITHUB_TOKEN to raise it from 60/h to 5000/h."
            )
        raise Failure(f"HTTP {exc.code} from {url}{detail}") from exc
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise Failure(f"could not reach {url}: {exc}") from exc


def http_json(url: str, headers: dict[str, str] | None = None):
    raw = http_get(url, headers)
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise Failure(f"{url} did not return JSON: {exc}") from exc


def github_headers() -> dict[str, str]:
    hdr = {"Accept": "application/vnd.github+json"}
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        hdr["Authorization"] = f"Bearer {token}"
    return hdr


# --------------------------------------------------------------------------
# Reading the tree
# --------------------------------------------------------------------------


def tracked_files(glob: str) -> list[str]:
    """Tracked files matching a glob. `git ls-files` on purpose: it honours
    .gitignore and never sees an untracked scratch file, which is the same
    convention scripts/check-spdx-headers.sh uses."""
    out = subprocess.run(
        ["git", "ls-files", "--", glob], capture_output=True, text=True, check=True
    ).stdout.split()
    return sorted(out)


def read_regex(read: dict) -> list[tuple[str, str]]:
    """-> [(value, 'file (xN)'), ...]. Raises Failure if ANY source is empty."""
    found: list[tuple[str, str]] = []
    for src in read["sources"]:
        glob, pattern = src["files"], src["pattern"]
        files = tracked_files(glob)
        if not files:
            raise Failure(f"glob `{glob}` matched no tracked file")
        rx = re.compile(pattern)
        hits = 0
        for path in files:
            try:
                text = Path(path).read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError) as exc:
                raise Failure(f"could not read {path}: {exc}") from exc
            matches = rx.findall(text)
            if matches:
                hits += len(matches)
                for m in matches:
                    value = m if isinstance(m, str) else m[0]
                    found.append((value.strip().strip("\"'"), path))
        if hits == 0:
            raise Failure(
                f"pattern for `{glob}` matched nothing in {len(files)} file(s) -- "
                "the pin moved or was renamed; fix the inventory rather than "
                "letting this entry silently report nothing"
            )
    return found


def read_json_path(read: dict) -> list[tuple[str, str]]:
    path = Path(read["file"])
    if not path.is_file():
        raise Failure(f"{path} does not exist")
    doc = json.loads(path.read_text(encoding="utf-8"))
    cur = doc
    for part in read["path"].split("."):
        if not isinstance(cur, dict) or part not in cur:
            raise Failure(f"{path}: no such path `{read['path']}` (stopped at `{part}`)")
        cur = cur[part]
    return [(str(cur), str(path))]


def read_command(read: dict) -> tuple[str, str]:
    """-> (detail, where). For package-manager aggregates."""
    cmd = read["cmd"]
    if shutil.which(cmd[0]) is None:
        raise Failure(
            f"`{cmd[0]}` is not on PATH, so this class went unchecked. "
            "That is not the same as up to date."
        )
    cwd = read.get("cwd", ".")
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    blob = proc.stdout + proc.stderr
    if proc.returncode != 0:
        head = " / ".join(blob.strip().splitlines()[-3:]) or "(no output)"
        raise Failure(f"`{' '.join(cmd)}` in {cwd} exited {proc.returncode}: {head}")
    unit = read.get("unit", "behind latest")
    if read.get("pattern"):
        m = re.search(read["pattern"], blob)
        n = int(m.group(1)) if m else 0
    else:
        n = len(re.findall(read["countPattern"], blob))
    return (read["zeroText"] if n == 0 else f"{n} {unit}"), f"`{' '.join(cmd)}` in {cwd}/"


# --------------------------------------------------------------------------
# Asking upstream
# --------------------------------------------------------------------------


def helm_index_versions(index: str, chart: str) -> list[str]:
    """Minimal, deliberately narrow parse of a Helm repository index.yaml.

    `helm repo index` always emits `entries: <chart>: - <keys at 4 columns>`,
    so anchoring on exact indentation separates the chart entry's own
    `version:` from a dependency's nested one. No YAML library is used: this
    script must run from a bare checkout with nothing but python3.
    """
    versions: list[str] = []
    in_entries = False
    in_chart = False
    for line in index.splitlines():
        if not in_entries:
            if line.rstrip() == "entries:":
                in_entries = True
            continue
        if line and not line[0].isspace():  # back to a top-level key
            break
        # A chart name: a 2-space key with no value. The `(?!- )` is
        # load-bearing -- a list item whose first key has no inline value
        # ("  - annotations:") is indented identically, and treating it as a
        # chart name silently switched the parser off for every repo whose
        # entries begin that way.
        m = re.match(r"^ {2}(?!- )([^:\s][^:]*):\s*$", line)
        if m:
            in_chart = m.group(1).strip("\"'") == chart
            continue
        if in_chart:
            m = re.match(r"^(?: {4}|  - )version:\s*(\S+)\s*$", line)
            if m:
                versions.append(m.group(1).strip("\"'"))
    return versions


def newest(candidates: list[str], allow_prerelease: bool = False) -> str:
    usable = [c for c in candidates if allow_prerelease or not PRERELEASE.search(c)]
    if not usable:
        raise Failure(
            f"upstream returned {len(candidates)} tag(s) but none were a usable "
            "release after dropping prereleases"
        )
    return max(usable, key=numeric_key)


def numeric_key(value: str) -> tuple[int, ...]:
    return tuple(int(x) for x in re.findall(r"\d+", value)) or (-1,)


def ask_helm(up: dict) -> str:
    index = http_get(up["repo"].rstrip("/") + "/index.yaml").decode("utf-8", "replace")
    versions = helm_index_versions(index, up["chart"])
    if not versions:
        raise Failure(
            f"no versions for chart `{up['chart']}` in {up['repo']}/index.yaml "
            f"({len(index)} bytes) -- the chart was renamed, the repo moved, or "
            "the response was not an index"
        )
    return newest(versions)


def ask_oci_helm(up: dict) -> str:
    registry, repo = up["registry"], up["repository"]
    tok = http_json(f"https://{registry}/token?scope=repository:{repo}:pull&service={registry}")
    token = tok.get("token") or tok.get("access_token")
    if not token:
        raise Failure(f"{registry} issued no anonymous pull token for {repo}")
    doc = http_json(
        f"https://{registry}/v2/{repo}/tags/list?n=1000", {"Authorization": f"Bearer {token}"}
    )
    tags = doc.get("tags") or []
    if not tags:
        raise Failure(f"{registry}/{repo} listed no tags")
    return newest(tags)


def ask_github_release(up: dict) -> str:
    doc = http_json(
        f"https://api.github.com/repos/{up['repo']}/releases/latest", github_headers()
    )
    tag = doc.get("tag_name")
    if not tag:
        raise Failure(f"{up['repo']} has no `latest` release (only tags, or none published)")
    return tag


def ask_github_branch(up: dict) -> str:
    doc = http_json(
        f"https://api.github.com/repos/{up['repo']}/commits/{up['branch']}", github_headers()
    )
    when = doc.get("commit", {}).get("committer", {}).get("date")
    if not when:
        raise Failure(f"no head commit date for {up['repo']}@{up['branch']}")
    return str(int(datetime.strptime(when, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC).timestamp()))


def ask_docker_hub(up: dict) -> str:
    rx = re.compile(up["tagPattern"])
    url = f"https://hub.docker.com/v2/repositories/{up['repository']}/tags?page_size=100"
    matched: list[str] = []
    seen = 0
    for _ in range(3):  # up to 300 most-recent tags
        doc = http_json(url)
        names = [r["name"] for r in doc.get("results", [])]
        seen += len(names)
        matched += [n for n in names if rx.match(n)]
        url = doc.get("next")
        if not url:
            break
    if not matched:
        raise Failure(
            f"none of the {seen} most-recent tags of {up['repository']} matched "
            f"`{up['tagPattern']}` -- the tag scheme changed"
        )
    return newest(matched)


def ask_crates_io(up: dict) -> str:
    doc = http_json(f"https://crates.io/api/v1/crates/{up['crate']}")
    version = doc.get("crate", {}).get("max_stable_version")
    if not version:
        raise Failure(f"crates.io returned no max_stable_version for {up['crate']}")
    return version


def ask_http_text(up: dict) -> str:
    body = http_get(up["url"]).decode("utf-8", "replace").strip()
    m = re.search(up["pattern"], body)
    if not m:
        raise Failure(f"{up['url']} did not match `{up['pattern']}` (got {body[:60]!r})")
    return m.group(1)


ASK = {
    "helm": ask_helm,
    "oci-helm": ask_oci_helm,
    # Same registry API, for an OCI artifact that is not a Helm chart (the dev
    # container features): the tag list of any OCI repository, newest wins.
    "oci": ask_oci_helm,
    "github-release": ask_github_release,
    "github-branch": ask_github_branch,
    "docker-hub": ask_docker_hub,
    "crates-io": ask_crates_io,
    "http-text": ask_http_text,
}


# --------------------------------------------------------------------------
# Stage 2: gates
# --------------------------------------------------------------------------


class GateResult:
    def __init__(self, outcome: str, detail: str = "", proves: str = ""):
        self.outcome = outcome  # pass | fail | unavailable | not-run | none
        self.detail = detail
        self.proves = proves

    def cell(self) -> str:
        if self.outcome == "pass":
            return f"green -- proves: {self.proves}"
        if self.outcome == "fail":
            return f"**RED** -- {self.detail}"
        if self.outcome == "unavailable":
            return f"could not run: {self.detail}"
        if self.outcome == "none":
            return f"no gate: {self.detail}"
        return self.detail or "not run"


def run(cmd: list[str], cwd: str | None = None, env: dict | None = None, timeout: int = 600):
    return subprocess.run(
        cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout
    )


def tail(proc: subprocess.CompletedProcess, n: int = 4) -> str:
    lines = [x for x in (proc.stdout + proc.stderr).splitlines() if x.strip()]
    return " / ".join(lines[-n:])[:400] or "(no output)"


def fetch_tool(url: str, member: str, dest: Path) -> Path:
    """Download a linux-amd64 release tarball and extract one binary."""
    blob = http_get(url)
    tgz = dest / "tool.tgz"
    tgz.write_bytes(blob)
    with tarfile.open(tgz) as tf:
        try:
            src = tf.extractfile(member)
        except KeyError as exc:
            raise Failure(f"{url} has no member `{member}`") from exc
        if src is None:
            raise Failure(f"{url} has no member `{member}`")
        out = dest / Path(member).name
        out.write_bytes(src.read())
    out.chmod(0o755)
    return out


def gate_helm_template(pin: dict, candidate: str, tmp: Path) -> GateResult:
    gate = pin["gate"]
    proves = gate["proves"]
    if shutil.which("cue") is None or shutil.which("helm") is None:
        return GateResult("unavailable", "cue and helm must both be on PATH", proves)
    comp = gate["component"]
    values = run(
        ["cue", "eval", "./cue/...", "-e", f'_components["{comp}"].values', "--out", "yaml"],
        cwd="platform-stack",
    )
    if values.returncode != 0:
        return GateResult("unavailable", f"cue eval of the component values failed: {tail(values)}", proves)
    vf = tmp / f"{comp}-values.yaml"
    vf.write_text(values.stdout, encoding="utf-8")

    up = pin["upstream"]
    if gate.get("oci"):
        chart_ref = [f"oci://{up['registry']}/{up['repository']}"]
    else:
        chart_ref = [up["chart"], "--repo", up["repo"]]
    proc = run(
        ["helm", "template", comp, *chart_ref, "--version", candidate, "-f", str(vf)],
        timeout=300,
    )
    if proc.returncode != 0:
        return GateResult("fail", f"`helm template` at {candidate} failed: {tail(proc)}", proves)
    if not proc.stdout.strip():
        return GateResult("fail", f"`helm template` at {candidate} rendered nothing", proves)
    return GateResult("pass", "", proves)


def gate_url_exists(pin: dict, candidate: str, tmp: Path) -> GateResult:
    gate = pin["gate"]
    url = gate["url"].format(version=candidate)
    try:
        http_get(url)
    except Failure as exc:
        return GateResult("fail", f"{exc}", gate["proves"])
    return GateResult("pass", "", gate["proves"])


def gate_cue_vet(pin: dict, candidate: str, tmp: Path) -> GateResult:
    proves = pin["gate"]["proves"]
    if sys.platform != "linux":
        return GateResult("unavailable", f"the candidate download is linux-amd64 only (host: {sys.platform})", proves)
    url = (
        f"https://github.com/cue-lang/cue/releases/download/{candidate}/"
        f"cue_{candidate}_linux_amd64.tar.gz"
    )
    try:
        binary = fetch_tool(url, "cue", tmp)
    except Failure as exc:
        return GateResult("unavailable", str(exc), proves)
    env = {**os.environ, "PATH": f"{binary.parent}:{os.environ.get('PATH', '')}"}
    # lint-cue.sh resolves cue via `command -v cue`, so a PATH prefix is all
    # it takes to run the whole CUE gate under the candidate.
    proc = run(["bash", "scripts/lint-cue.sh"], env=env, timeout=600)
    if proc.returncode != 0:
        return GateResult("fail", f"scripts/lint-cue.sh under cue {candidate}: {tail(proc)}", proves)
    return GateResult("pass", "", proves)


def gate_helm_lint(pin: dict, candidate: str, tmp: Path) -> GateResult:
    proves = pin["gate"]["proves"]
    if sys.platform != "linux":
        return GateResult("unavailable", f"the candidate download is linux-amd64 only (host: {sys.platform})", proves)
    if shutil.which("make") is None or shutil.which("cue") is None:
        return GateResult("unavailable", "make and cue must both be on PATH", proves)
    ver = candidate if candidate.startswith("v") else f"v{candidate}"
    try:
        binary = fetch_tool(f"https://get.helm.sh/helm-{ver}-linux-amd64.tar.gz", "linux-amd64/helm", tmp)
    except Failure as exc:
        return GateResult("unavailable", str(exc), proves)
    env = {**os.environ, "PATH": f"{binary.parent}:{os.environ.get('PATH', '')}"}
    proc = run(["make", "-C", "platform-stack", "render"], env=env, timeout=600)
    if proc.returncode != 0:
        return GateResult("fail", f"`make -C platform-stack render` under helm {ver}: {tail(proc)}", proves)
    return GateResult("pass", "", proves)


GATES = {
    "helm-template": gate_helm_template,
    "url-exists": gate_url_exists,
    "cue-vet": gate_cue_vet,
    "helm-lint": gate_helm_lint,
}


# --------------------------------------------------------------------------
# One entry
# --------------------------------------------------------------------------


class Row:
    def __init__(self, pin: dict):
        self.pin = pin
        self.id = pin["id"]
        self.title = pin["title"]
        self.cls = pin["class"]
        self.status = ""
        self.current = ""
        self.available = ""
        self.detail = ""
        self.where: list[str] = []
        self.drift: list[str] = []
        self.error = ""
        self.gate = GateResult("not-run")

    @property
    def note(self) -> str:
        return self.pin.get("note") or ""


LOCATION_CAP = 6


def locations(found: list[tuple[str, str]]) -> list[str]:
    """File list for the report, capped. actions/checkout alone occurs in 30
    files; printing all of them makes the row unreadable and pushes the body
    towards GitHub's 65536-character issue limit. Run the script to see the
    full list."""
    counts: dict[str, int] = {}
    for _, path in found:
        counts[path] = counts.get(path, 0) + 1
    cells = [f"`{p}`" + (f" x{n}" if n > 1 else "") for p, n in sorted(counts.items())]
    if len(cells) > LOCATION_CAP:
        hidden = len(cells) - LOCATION_CAP
        cells = cells[:LOCATION_CAP] + [f"+{hidden} more file(s)"]
    return cells


def as_date(unix_str: str) -> str:
    return datetime.fromtimestamp(int(unix_str), tz=timezone.utc).strftime("%Y-%m-%d")


def process(pin: dict, do_gates: bool, tmp: Path) -> Row:
    row = Row(pin)
    read = pin["read"]

    # --- current value(s) out of the tree
    try:
        if read["kind"] == "command":
            row.status = AGGREGATE
            row.detail, where = read_command(read)
            row.where = [where]
            return row
        found = read_regex(read) if read["kind"] == "regex" else read_json_path(read)
    except Failure as exc:
        row.status = TOOL_MISSING if read["kind"] == "command" else UNREADABLE
        row.error = str(exc)
        return row

    values = sorted({v for v, _ in found})
    row.where = locations(found)
    is_time = read.get("format") == "unixtime"
    row.current = ", ".join(as_date(v) for v in values) if is_time else ", ".join(values)
    if len(values) > 1 and not pin.get("multipleValuesOk"):
        # More than one value for one decision is a bug -- unless the entry
        # says otherwise. Without the opt-out, three entries that hold
        # deliberately different image flavours (alpine vs debian builders,
        # static vs nodejs distroless) sat permanently in the drift table and
        # would have trained the reader to scroll past it.
        row.drift = values

    # --- newest upstream
    up = pin["upstream"]
    if any("{major}" in v for v in up.values() if isinstance(v, str)):
        majors = sorted({str(numeric_key(v)[0]) for v in values})
        if len(majors) != 1 or majors[0] == "-1":
            row.status = UNREADABLE
            row.error = (
                f"upstream uses {{major}} but the tree holds {len(majors)} majors "
                f"({', '.join(values)}) -- the placeholder needs exactly one"
            )
            return row
        up = {k: v.replace("{major}", majors[0]) if isinstance(v, str) else v for k, v in up.items()}
    if up["kind"] == "none":
        row.status = FLOATING
        row.available = "n/a"
    else:
        try:
            latest = ASK[up["kind"]](up)
        except Failure as exc:
            row.status = UNREACHABLE
            row.error = str(exc)
            return row
        row.available = as_date(latest) if is_time else latest
        compare = pin["compare"]
        if compare == "none":
            row.status = FLOATING
        elif compare == "major":
            behind = numeric_key(latest)[:1] > numeric_key(max(values, key=numeric_key))[:1]
            row.status = BEHIND if behind else CURRENT
        elif compare == "minor":
            # The OLDEST value, as under `numeric`: one stale copy is enough.
            oldest = min(values, key=numeric_key)
            row.status = BEHIND if numeric_key(latest)[:2] > numeric_key(oldest)[:2] else CURRENT
        else:
            oldest = min(values, key=numeric_key)
            row.status = BEHIND if numeric_key(latest) > numeric_key(oldest) else CURRENT
        if is_time and row.status == BEHIND:
            days = (int(latest) - min(int(v) for v in values)) // 86400
            row.detail = f"{days} days behind"
        # A held pin keeps its NUMERIC compare on purpose. The tempting
        # `compare: "none"` would park it in the floating section forever, and
        # then the row would never flip when the blocker finally expires --
        # which is precisely the event this class exists to make loud. Holding
        # only diverts a row that is CURRENT; the moment upstream releases past
        # it the row is BEHIND and leaves for the behind table on its own.
        if pin.get("held") and row.status == CURRENT:
            row.status = HELD

    # --- stage 2
    gate = pin.get("gate")
    if gate is None:
        row.gate = GateResult("none", pin.get("gateGap", "not stated -- fix the inventory"))
    elif row.status != BEHIND:
        row.gate = GateResult("not-run", "nothing to try (not behind)")
    elif not do_gates:
        row.gate = GateResult("not-run", "gates disabled (--no-gates)")
    else:
        candidate = row.available
        try:
            row.gate = GATES[gate["kind"]](pin, candidate, tmp)
        except Failure as exc:
            row.gate = GateResult("unavailable", str(exc), gate["proves"])
        except subprocess.TimeoutExpired:
            row.gate = GateResult("unavailable", "the gate timed out", gate["proves"])
    return row


# --------------------------------------------------------------------------
# Report
# --------------------------------------------------------------------------


def table(head: list[str], rows: list[list[str]]) -> list[str]:
    out = ["| " + " | ".join(head) + " |", "|" + "|".join(["---"] * len(head)) + "|"]
    out += ["| " + " | ".join(c.replace("|", "\\|") for c in r) + " |" for r in rows]
    return out + [""]


def render(rows: list[Row], inv: dict, gates_on: bool) -> str:
    errors = [r for r in rows if r.status in ERROR_STATUSES]
    drifted = [r for r in rows if r.drift]
    behind = [r for r in rows if r.status == BEHIND]
    current = [r for r in rows if r.status == CURRENT]
    held = [r for r in rows if r.status == HELD]
    floating = [r for r in rows if r.status == FLOATING]
    aggregates = [r for r in rows if r.status == AGGREGATE]
    red = [r for r in behind if r.gate.outcome == "fail"]
    green = [r for r in behind if r.gate.outcome == "pass"]

    L: list[str] = []
    L.append("# Upstream version watch")
    L.append("")
    L.append(
        f"Generated {datetime.now(UTC).strftime('%Y-%m-%d %H:%M UTC')} by "
        f"`scripts/upstream-versions.py` from `{INVENTORY}`. "
        "First-party versions are deliberately absent -- release workflows move those."
    )
    L.append("")
    L.append(
        f"**{len(rows)} entries: {len(behind)} behind, {len(current)} up to date, "
        f"{len(held)} held, "
        f"{len(drifted)} drifted, {len(floating)} floating/manual, "
        f"{len(aggregates)} aggregates, {len(errors)} could not be checked.** "
        f"Of the {len(behind)} behind, {len(green)} passed their gate and {len(red)} went red"
        + ("" if gates_on else " (gates were disabled for this run)")
        + "."
    )
    L.append("")

    L.append("## Could not be checked")
    L.append("")
    if errors:
        L.append(
            "These are **not** up to date and **not** behind -- they are unknown. "
            "This section is why the job exits non-zero."
        )
        L.append("")
        L += table(
            ["pin", "status", "what went wrong"],
            [[f"`{r.id}`", r.status, r.error] for r in errors],
        )
    else:
        L.append("None -- every entry in the inventory was read and resolved against its upstream.")
        L.append("")

    L.append("## Drift: one decision, more than one value in the tree")
    L.append("")
    if drifted:
        L.append(
            "Nothing else in CI detects these. They are bugs independent of whether "
            "an upgrade exists."
        )
        L.append("")
        L += table(
            ["pin", "values found", "where"],
            [[f"`{r.id}`", ", ".join(f"`{v}`" for v in r.drift), ", ".join(r.where)] for r in drifted],
        )
    else:
        L.append("None.")
        L.append("")

    L.append("## Behind upstream")
    L.append("")
    if behind:
        L += table(
            ["pin", "current", "available", "gate", "where"],
            [
                [
                    f"`{r.id}`<br>{r.title}",
                    f"`{r.current}`" + (f" ({r.detail})" if r.detail else ""),
                    f"`{r.available}`",
                    r.gate.cell(),
                    ", ".join(r.where),
                ]
                for r in sorted(behind, key=lambda r: (r.cls, r.id))
            ],
        )
        notes = [r for r in behind if r.note]
        if notes:
            L.append("### Notes on the rows above")
            L.append("")
            L += [f"- **`{r.id}`** -- {r.note}" for r in notes]
            L.append("")
    else:
        L.append("None.")
        L.append("")

    L.append("## Held: on the newest release and still a dead end")
    L.append("")
    L.append(
        "NOT behind, and NOT fine. Each of these is on its newest published version "
        "and stuck there for a stated reason -- an upstream blocker, an abandoned "
        "project, a fork nobody has picked. A check that asks only *are we behind* "
        "reports nothing for this class, which is how an abandoned dependency comes "
        "to read healthier here than a maintained one that ships every week. "
        "When `upstream newest` moves past `in tree` the row leaves this section for "
        "the behind table -- that is the blocker expiring, and it is the event this "
        "section exists to make loud."
    )
    L.append("")
    L += (
        table(
            ["pin", "in tree", "upstream newest", "why it is held"],
            [[f"`{r.id}`", f"`{r.current}`", f"`{r.available}`", r.pin.get("heldReason", "-")] for r in held],
        )
        if held
        else ["None.", ""]
    )

    L.append("## Up to date")
    L.append("")
    L += (
        table(["pin", "version", "where"], [[f"`{r.id}`", f"`{r.current}`", ", ".join(r.where)] for r in current])
        if current
        else ["None.", ""]
    )

    L.append("## Floating by design, or needing a human")
    L.append("")
    L += (
        table(
            ["pin", "in tree", "upstream newest", "why it is not compared"],
            [[f"`{r.id}`", f"`{r.current}`", f"`{r.available}`", r.note or "-"] for r in floating],
        )
        if floating
        else ["None.", ""]
    )

    L.append("## Package-manager aggregates")
    L.append("")
    L.append(
        "Reported as counts, not rows: cargo and bun ship better readers than anything "
        "written here would be, and 300 dependency rows would bury the 50 pins above."
    )
    L.append("")
    L += (
        table(["set", "state", "read by"], [[r.title, r.detail, r.where[0]] for r in aggregates])
        if aggregates
        else ["None.", ""]
    )

    L.append("## Test gaps: classes with no cluster-free gate")
    L.append("")
    L.append(
        "Every entry below is reported without being tested, and this is the list of "
        "reasons. A green that is not in this table was actually run."
    )
    L.append("")
    gaps: dict[str, list[str]] = {}
    for r in rows:
        if r.pin.get("gate") is None and r.pin.get("gateGap"):
            gaps.setdefault(r.pin["gateGap"], []).append(r.id)
    L += table(
        ["entries", "why no gate"],
        [[", ".join(f"`{i}`" for i in sorted(ids)), why] for why, ids in sorted(gaps.items(), key=lambda kv: kv[1])],
    )

    if inv.get("notCoveredClasses"):
        L.append("## Deliberately outside the inventory")
        L.append("")
        L += table(
            ["class", "where", "why"],
            [[c["class"], f"`{c['where']}`", c["why"]] for c in inv["notCoveredClasses"]],
        )

    return "\n".join(L) + "\n"


# --------------------------------------------------------------------------


def validate_inventory(pins: list[dict]) -> list[str]:
    """Fail fast on an entry that would report something meaningless.

    Every rule here exists because breaking it produces a row that LOOKS like
    an answer: a gateless entry with no stated reason prints an empty excuse,
    and a `compare: none` entry with no note is indistinguishable from a pin
    nobody is watching.
    """
    problems: list[str] = []
    seen: set[str] = set()
    for pin in pins:
        pid = pin.get("id", "<no id>")
        if pid in seen:
            problems.append(f"{pid}: duplicate id")
        seen.add(pid)
        for field in ("id", "class", "title", "read", "upstream", "compare"):
            if not pin.get(field):
                problems.append(f"{pid}: missing `{field}`")
        if pin.get("gate") is None and not pin.get("gateGap"):
            problems.append(f"{pid}: no gate and no gateGap saying why")
        if pin.get("compare") and pin.get("compare") not in COMPARES:
            # Without this a typo fell through to the numeric branch and
            # reported as if it had been meant.
            problems.append(f"{pid}: unknown compare `{pin.get('compare')}` (one of {', '.join(sorted(COMPARES))})")
        if pin.get("compare") == "none" and not pin.get("note"):
            problems.append(f"{pid}: compare `none` needs a note saying why it floats")
        up = pin.get("upstream", {})
        if up.get("kind") not in ASK and up.get("kind") != "none":
            problems.append(f"{pid}: unknown upstream kind `{up.get('kind')}`")
        gate = pin.get("gate")
        if gate and gate.get("kind") not in GATES:
            problems.append(f"{pid}: unknown gate kind `{gate.get('kind')}`")
        if gate and not gate.get("proves"):
            problems.append(f"{pid}: gate must say what a green proves")
    return problems


def read_only(pins: list[dict]) -> int:
    """Stage 0 alone: can every entry still READ its value out of the tree?

    The cheap half of the watcher's contract, runnable offline after any edit
    that moves a pin -- the failure it exists for is a pattern that quietly
    stopped matching, which a full run would also report, but only after
    asking fifty upstreams. `command` readers run a package manager rather
    than read text, so they are listed as not read instead of being run.
    """
    unreadable = 0
    for pin in pins:
        read = pin["read"]
        if read["kind"] == "command":
            print(f"SKIP        {pin['id']}: `command` reader -- runs `{' '.join(read['cmd'])}`, not a tree read")
            continue
        try:
            found = read_regex(read) if read["kind"] == "regex" else read_json_path(read)
        except Failure as exc:
            unreadable += 1
            print(f"{UNREADABLE}  {pin['id']}: {exc}")
            continue
        values = sorted({v for v, _ in found})
        drift = len(values) > 1 and not pin.get("multipleValuesOk")
        print(
            f"{'DRIFT' if drift else 'READ':<11} {pin['id']}: {', '.join(values)} "
            f"({len(found)} site(s) in {len({p for _, p in found})} file(s))"
        )
    sys.stdout.flush()
    print(f"\n{len(pins)} entries, {unreadable} unreadable.", file=sys.stderr)
    return 1 if unreadable else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--no-gates", action="store_true", help="stage 1 only: do not try candidates")
    ap.add_argument(
        "--read-only",
        action="store_true",
        help="read every pin out of the tree and stop: no upstream query, no gate, no network",
    )
    ap.add_argument("--only", action="append", default=[], metavar="ID", help="restrict to these pin ids")
    ap.add_argument("-o", "--output", metavar="FILE", help="write the report here instead of stdout")
    args = ap.parse_args()

    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    ).stdout.strip()
    os.chdir(root)

    inv = json.loads(Path(INVENTORY).read_text(encoding="utf-8"))
    problems = validate_inventory(inv["pins"])
    if problems:
        print(f"ERROR: {INVENTORY} is malformed:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 2

    pins = inv["pins"]
    if args.only:
        pins = [p for p in pins if p["id"] in args.only]
        missing = set(args.only) - {p["id"] for p in pins}
        if missing:
            print(f"no such pin id: {', '.join(sorted(missing))}", file=sys.stderr)
            return 2
    if not pins:
        # The inventory becoming empty must never read as "all clear".
        print(f"ERROR: {INVENTORY} declares no pins.", file=sys.stderr)
        return 2

    if args.read_only:
        if args.output:
            # A report file that is silently never written is worse than an error.
            print("--read-only prints to stdout and writes no report; drop -o", file=sys.stderr)
            return 2
        return read_only(pins)

    rows: list[Row] = []
    with tempfile.TemporaryDirectory(prefix="upstream-watch-") as td:
        for i, pin in enumerate(pins, 1):
            print(f"[{i}/{len(pins)}] {pin['id']}", file=sys.stderr, flush=True)
            rows.append(process(pin, not args.no_gates, Path(td)))

    report = render(rows, inv, not args.no_gates)
    if args.output:
        Path(args.output).write_text(report, encoding="utf-8")
    else:
        sys.stdout.write(report)

    errors = [r for r in rows if r.status in ERROR_STATUSES]
    behind = [r for r in rows if r.status == BEHIND]
    red = [r for r in behind if r.gate.outcome == "fail"]
    print(
        f"\n{len(rows)} entries: {len(behind)} behind ({len(red)} gate-red), "
        f"{len(errors)} unchecked.",
        file=sys.stderr,
    )
    if errors:
        # Exit non-zero ONLY for this script's own failures. An available
        # upgrade -- even one whose gate went red -- is the output, not a fault.
        for r in errors:
            print(f"  {r.status}: {r.id}: {r.error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
