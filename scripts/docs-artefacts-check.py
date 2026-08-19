# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""Pass 2 of the documentation gate: what the two build hooks left behind.

Run from the repository root with the built site as the only argument;
`scripts/docs-check.sh` does that, and nothing else should need to.

This lives in its own file rather than inside a heredoc in that script
for a reason worth keeping: the script runs its body through
`bash -c '...'`, and an apostrophe anywhere in an embedded heredoc closes
that string. The truncated remainder was still valid Python (it ended
inside a comment), so the pass exited 0 having asserted nothing and the
two `docsgen` passes after it never ran at all. A gate that can be
switched off by a possessive is not a gate.
"""

import html
import re
import sys
from pathlib import Path
from urllib.parse import urlparse

from mkdocs.config import load_config

site = Path(sys.argv[1])
# `or ""` because a missing `site_url:` leaves this None, and every
# comparison below would then raise a TypeError instead of reporting
# the one thing that is wrong. The hook refuses to run without it (see
# `on_post_build`); this is the second line of defence, and it has to
# survive long enough to say so.
base = load_config("mkdocs.yml")["site_url"] or ""
problems = []
lexer_problems = []

# A twin must not be a stub. The floor is half the rendered page's own
# visible text — deliberately generous, because a twin is markdown and
# runs a little LONGER than the text it renders to (a link carries its
# target), so the healthy ratio is above 1 and nothing sits near 0.5.
# A truncating hook lands orders of magnitude below it.
SUBSTANCE_FLOOR = 0.5

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
    # comma in a list of five. Deduplicated because some callers count
    # fences and report the pages they sit on, and one page can hold
    # several.
    return ", ".join(p or "<site root>" for p in sorted(set(paths))[:5])


def twin_for(url):
    """Where the twin of a published page belongs, from the page's URL alone."""
    return "index.md" if url == "" else url.rstrip("/") + ".md"


def visible(fragment):
    """Rendered HTML to the text a reader sees, whitespace collapsed."""
    return re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", " ", fragment))).strip()


def rendered_text(url):
    """The page's own body text — the theme chrome is outside <article>."""
    body = re.search(
        r"<article[^>]*>(.*?)</article>", (site / url / "index.html").read_text(), re.S
    )
    return visible(body.group(1)) if body else ""


index_path = site / "llms.txt"
index = ""
if not index_path.is_file():
    problems.append("llms.txt was not written")
else:
    index = index_path.read_text()
    if "CC-BY-4.0" not in index:
        problems.append("llms.txt carries no licence line (nothing in it names CC-BY-4.0)")

urls = re.findall(r"(?m)^- \[[^\]]*\]\((\S+)\)", index)
listed = set()
# Two separate properties, because `not u.startswith(base)` alone is
# vacuous: `base` comes out of the same config the hook read, so it is a
# prefix of everything the hook wrote and the test can never fail. The
# scheme is checked against the world instead of against the config.
unscheme = [u for u in urls if urlparse(u).scheme not in ("http", "https")]
if unscheme:
    problems.append(
        "llms.txt links are not absolute URLs (no http/https scheme): " + show(unscheme)
    )
if not base:
    problems.append(
        "mkdocs.yml declares no usable `site_url:`, so every URL in llms.txt and"
        " every `url:` in llms-full.txt is a bare path or the literal string None"
        " -- a fully publishable site whose machine-readable layer is unusable"
    )
else:
    offsite = [u for u in urls if not u.startswith(base)]
    if offsite:
        problems.append("llms.txt links outside site_url (" + base + "): " + show(offsite))
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

# How much page there is to carry, per URL and in total. Everything
# below that asks "is this substantial?" measures against THIS, so the
# expectation grows with the corpus and is never a recorded number.
rendered = {u: len(rendered_text(u)) for u in published}
rendered_total = sum(rendered.values())

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
    # Emptiness is not the only way to stop delivering. A hook that
    # writes `doc.markdown[:40]` passes every count above and every
    # entry is non-empty, while the bundle collapses to one truncated
    # heading per page. So the bodies are weighed against the site.
    body_total = sum(len(b.strip()) for _, b in bundled)
    if rendered_total and body_total < SUBSTANCE_FLOOR * rendered_total:
        problems.append(
            "llms-full.txt holds " + str(body_total) + " characters of page body for a"
            + " site that renders " + str(rendered_total) + " -- below the floor of "
            + str(SUBSTANCE_FLOOR) + ", so the bundle is truncated rather than absent"
        )

# Twins, as a SET against the pages the site published: two equal
# counts can hide a missing twin behind a stray .md somewhere else.
expected_twins = {twin_for(u) for u in published}
twins = {p.relative_to(site).as_posix() for p in site.rglob("*.md")}
if expected_twins - twins:
    problems.append(
        str(len(expected_twins - twins)) + " published page(s) have no markdown twin: "
        + show(expected_twins - twins)
    )
if twins - expected_twins:
    problems.append(
        str(len(twins - expected_twins)) + " markdown file(s) in the site match no"
        + " published page: " + show(twins - expected_twins)
    )
thin = [
    u
    for u in published
    if twin_for(u) in twins
    and rendered[u]
    and len((site / twin_for(u)).read_text().strip()) < SUBSTANCE_FLOOR * rendered[u]
]
if thin:
    problems.append(
        str(len(thin)) + " markdown twin(s) hold less than " + str(SUBSTANCE_FLOOR)
        + " of what their page renders -- present, counted, and truncated: " + show(thin)
    )

# --- the CUE lexer, read off the rendered page rather than the registry.
#
# The expectation is derived twice over and stated nowhere: which fences
# are CUE comes from each page's own twin (which is its source
# markdown), and what "highlighted" means comes from the block the build
# produced for it. So this covers every ```cue fence the site has today
# and every one added tomorrow, in any language-agnostic way -- there is
# no list of pages and no count here to keep in step.
FENCE = re.compile(r"^(?P<prefix>[ \t>]*)(?P<ticks>`{3,})(?P<info>[^`]*)$")
BLOCK = re.compile(r"<div class=\"highlight\"><pre>(.*?)</pre></div>", re.S)


def squash(text):
    """A whitespace-free signature, so a source fence can be matched to its block."""
    return re.sub(r"\s+", "", text)


def fences(markdown):
    """(language, body) per fenced block. Handles blockquoted and indented fences."""
    out = []
    lang = None
    ticks = 0
    quoted = False
    body = []
    for line in markdown.split("\n"):
        hit = FENCE.match(line)
        if lang is None:
            if hit:
                info = hit.group("info").split()
                lang = info[0] if info else ""
                ticks = len(hit.group("ticks"))
                quoted = ">" in hit.group("prefix")
                body = []
            continue
        if hit and len(hit.group("ticks")) >= ticks and not hit.group("info").strip():
            out.append((lang, "\n".join(body)))
            lang = None
            continue
        if quoted:
            line = re.sub(r"^[ \t]*> ?", "", line)
        body.append(line)
    return out


def code_blocks(page_html):
    """(signature, carries token spans) per highlighted block, in document order."""
    return [
        (squash(visible(inner)), "<span class=\"" in inner) for inner in BLOCK.findall(page_html)
    ]


checked = 0
unhighlighted = []
unrendered = []
for url in published:
    twin = site / twin_for(url)
    if not twin.is_file():
        continue
    blocks = code_blocks((site / url / "index.html").read_text())
    for lang, body in fences(twin.read_text()):
        if lang != "cue":
            continue
        checked += 1
        # `all`, not `any`: two fences on one page can share a body (a
        # licence header written once as CUE and once as Rust), and the
        # weaker test would let the styled twin vouch for the plain one.
        hits = [tokens for signature, tokens in blocks if signature == squash(body)]
        if not hits:
            unrendered.append(url)
        elif not all(hits):
            unhighlighted.append(url)

if not checked:
    lexer_problems.append(
        "not one ```cue fence was found on the whole site, so this check proved"
        " nothing. Either the corpus stopped writing CUE or the fences stopped"
        " reaching the twins -- a check that cannot fail is not a check"
    )
if unrendered:
    lexer_problems.append(
        str(len(unrendered)) + " ```cue fence(s) have no matching highlighted block on"
        + " their page at all, so pygments is not rendering code blocks here"
        + " (`use_pygments: false`, or a superfences change): " + show(unrendered)
    )
if unhighlighted:
    lexer_problems.append(
        str(len(unhighlighted)) + " ```cue fence(s) rendered with no syntax tokens --"
        + " they are plain text on the published page: " + show(unhighlighted)
    )

if problems or lexer_problems:
    if problems:
        sys.stderr.write("ERROR: docs/hooks/llm_export.py did not deliver:\n")
        for problem in problems:
            sys.stderr.write("  - " + problem + "\n")
    if lexer_problems:
        sys.stderr.write("ERROR: docs/hooks/cue_lexer.py did not take:\n")
        for problem in lexer_problems:
            sys.stderr.write("  - " + problem + "\n")
        sys.stderr.write(
            "  The hook self-checks at on_config, but that assertion is inside the"
            " thing it guards -- deleting its line from `hooks:` in mkdocs.yml removes"
            " both. This check reads the built page instead.\n"
        )
    sys.stderr.write(
        "The mkdocs build stays GREEN whatever either hook does, so this pass is the"
        " only detector. Confirm both are still listed under `hooks:` in mkdocs.yml,"
        " then read the module docstring of the one named above.\n"
    )
    raise SystemExit(1)

print(
    "llm artefacts OK: " + str(len(listed)) + " page(s) indexed in llms.txt, "
    + str(len(published)) + " bundled in llms-full.txt, " + str(len(twins))
    + " markdown twin(s); CUE lexer OK: " + str(checked) + " highlighted fence(s)"
)
