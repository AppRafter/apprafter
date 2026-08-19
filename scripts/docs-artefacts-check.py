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

Every expectation here is derived from the OTHER SIDE
-----------------------------------------------------

All three artefacts (`llms.txt`, `llms-full.txt`, the markdown twins)
are written by `docs/hooks/llm_export.py` out of the committed source
pages. So each one is asserted against **that committed source**, read
back off disk with `git ls-files` and mkdocs' own front-matter parser —
never against another number the same hook produced, and never against
its own length.

That rule is the whole subject of this file, and it is written down
because every hole found here so far was one violation of it:

* the CUE lexer's only detector was the `on_config` self-check inside
  the hook it guarded, so deleting the hook deleted the detector;
* `llms.txt` links were checked for the `site_url` prefix taken from
  the same config that wrote them, so the test could not fail;
* a twin was checked for *length* rather than content, so padding it
  with filler passed, and so did serving one page's twin for another;
* the index entry was matched by a regex that did not require the
  `: description` suffix at all, so an entry line that dropped every
  authored description left this pass reporting OK.

A count is not an assertion either. Nothing below compares a number to
a number it also computed: the page set comes from the built site, the
content comes from the committed source, and the two are compared
element by element.
"""

import html
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import urlparse

from mkdocs.config import load_config
from mkdocs.utils import meta

site = Path(sys.argv[1])
config = load_config("mkdocs.yml")
# `or ""` because a missing `site_url:` leaves this None, and every
# comparison below would then raise a TypeError instead of reporting
# the one thing that is wrong. The hook refuses to run without it (see
# `on_post_build`); this is the second line of defence, and it has to
# survive long enough to say so.
base = config["site_url"] or ""
# `docs_dir` comes back absolute; git and the paths it prints are
# relative to the repository root, which is this script's CWD.
docs_dir = Path(config["docs_dir"]).resolve().relative_to(Path.cwd().resolve())
problems = []
lexer_problems = []


def show(paths):
    # The home page is the empty path; printed bare it reads as a stray
    # comma in a list of five. Deduplicated because some callers count
    # fences and report the pages they sit on, and one page can hold
    # several.
    return ", ".join(p or "<site root>" for p in sorted(set(paths))[:5])


def visible(fragment):
    """Rendered HTML to the text a reader sees, whitespace collapsed."""
    return re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", " ", fragment))).strip()


# --- the committed source, which is what every artefact is judged against.


def url_for(src_uri):
    """The site URL mkdocs gives a source page, from its path alone.

    The inverse of the twin rule below, and the same three cases:
    `index.md` and `README.md` are a directory's index page (mkdocs
    treats both stems that way), anything else gets a directory of its
    own. `use_directory_urls` is the default and this project's
    setting; the assertion that this mapping is right is that it must
    hit every page the build published, which is checked rather than
    assumed.
    """
    directory, _, name = src_uri.rpartition("/")
    if name[: -len(".md")] in ("index", "README"):
        return directory + "/" if directory else ""
    return src_uri[: -len(".md")] + "/"


def twin_for(url):
    """Where the twin of a published page belongs, from the page's URL alone."""
    return "index.md" if url == "" else url.rstrip("/") + ".md"


tracked = subprocess.run(
    ["git", "ls-files", "-z", "--", str(docs_dir)],
    capture_output=True,
    text=True,
    check=True,
).stdout.split("\0")
# {site URL: (source path, body with front matter stripped, description)}.
# Read with mkdocs' own `meta.get_data`, so "what the front matter said"
# means here exactly what it meant to the build.
source = {}
for path in tracked:
    if not path.endswith(".md"):
        continue
    src_uri = Path(path).relative_to(docs_dir).as_posix()
    body, front = meta.get_data(Path(path).read_text())
    description = front.get("description")
    source[url_for(src_uri)] = (
        src_uri,
        body.strip(),
        " ".join(str(description).split()) if description else "",
    )

# What the site actually published, as site-root-relative URL paths.
# Deriving the page SET from the build output is the whole point:
# a page added tomorrow is covered without editing this script.
published = sorted(
    "" if p.parent == site else p.parent.relative_to(site).as_posix() + "/"
    for p in site.rglob("index.html")
    if not p.relative_to(site).as_posix().startswith("assets/")
)
# The ADRs are bundled and twinned but deliberately not indexed --
# docs/hooks/llm_export.py says why.
indexable = [u for u in published if not u.startswith("adr/")]

orphan_pages = [u for u in published if u not in source]
if orphan_pages:
    problems.append(
        str(len(orphan_pages))
        + " published page(s) map to no committed source, so nothing here can say"
        + " what they were supposed to contain (either `url_for` no longer matches"
        + " how mkdocs names output, or the site is publishing untracked files): "
        + show(orphan_pages)
    )

# A page listed in `not_in_nav:` is DECLARED published-but-off-nav. A
# page dropped by `exclude_docs:` is not published at all. Both are
# authored in mkdocs.yml, and they contradict each other silently:
# mkdocs logs only INFO when an excluded page is linked (see
# scripts/docs-check.sh, which fails on that line), and logs NOTHING at
# all when nobody links it. `not_in_nav` is the only committed statement
# that these pages are meant to exist, so it is read as one.
not_in_nav = config["not_in_nav"]
declared = sorted(
    url
    for url, (src_uri, _, _) in source.items()
    if not_in_nav.match_file(src_uri) and url not in set(published)
)
if declared:
    problems.append(
        str(len(declared))
        + " page(s) are listed in `not_in_nav:` -- which declares them published but"
        + " off the nav -- and the build did not publish them at all. Something in"
        + " `exclude_docs:` is swallowing them, and mkdocs reports that as INFO or not"
        + " at all: " + show(declared)
    )

index_path = site / "llms.txt"
index = ""
if not index_path.is_file():
    problems.append("llms.txt was not written")
else:
    index = index_path.read_text()
    if "CC-BY-4.0" not in index:
        problems.append("llms.txt carries no licence line (nothing in it names CC-BY-4.0)")

# The label and the description are captured, not just the URL: an
# earlier version matched `^- \[[^\]]*\]\((\S+)\)` and so could not see
# an entry line that had dropped its `: description` suffix. Every
# authored description would have been gone from the index with this
# pass reporting OK. The suffix is OPTIONAL in the pattern so that its
# absence is reported as the missing description it is, rather than as
# a page missing from the index.
ENTRY = re.compile(r"(?m)^- \[(?P<label>[^\]]*)\]\((?P<url>[^)\s]+)\)(?::[ \t]*(?P<desc>.*))?$")
entries = [(m.group("url"), m.group("desc")) for m in ENTRY.finditer(index)]
urls = [url for url, _ in entries]
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
    # The index's ONE authored input, checked against where it was
    # authored. `_require_descriptions` in the hook fails the build when
    # a page's front matter has no description; nothing asserted that
    # the description reached the artefact, and the entry line can drop
    # it while every count above still balances.
    undescribed = []
    mismatched = []
    for url, desc in entries:
        if not url.startswith(base):
            continue
        page = url[len(base):]
        if page not in source:
            continue
        authored = source[page][2]
        if not (desc or "").strip():
            undescribed.append(page)
        elif desc.strip() != authored:
            mismatched.append(page)
    if undescribed:
        problems.append(
            str(len(undescribed))
            + " llms.txt entr(ies) carry no description, though the page's front matter"
            + " has one -- the index is a bare link list and its whole reason to exist"
            + " is gone: " + show(undescribed)
        )
    if mismatched:
        problems.append(
            str(len(mismatched))
            + " llms.txt entr(ies) carry a description that is not the one in the"
            + " page's front matter: " + show(mismatched)
        )

full_path = site / "llms-full.txt"
if not full_path.is_file():
    problems.append("llms-full.txt was not written")
else:
    bundled = re.findall(
        # `audience:` is [^\n]+ rather than \S+ so an author writing a
        # two-word audience does not read here as a missing page.
        r"(?ms)^---\nurl: (\S+)\n(description: [^\n]*\n)?audience: [^\n]+\n---\n(.*?)"
        r"(?=^---\nurl: |\Z)",
        full_path.read_text(),
    )
    if len(bundled) != len(published):
        problems.append(
            "llms-full.txt bundles " + str(len(bundled)) + " page(s); the site published "
            + str(len(published))
        )
    # Not "is it empty" and not "is it long enough" -- both of those
    # were passed by a writer that kept the first forty characters and
    # padded the rest with filler, and neither could see two pages'
    # bodies swapped for each other. The bundle carries the committed
    # page or it does not.
    wrong_body = []
    wrong_desc = []
    for url, desc_line, body in bundled:
        if not base or not url.startswith(base):
            continue
        page = url[len(base):]
        if page not in source:
            continue
        _, want_body, want_desc = source[page]
        if body.strip() != want_body:
            wrong_body.append(page)
        got_desc = desc_line[len("description: "):].strip() if desc_line else ""
        if got_desc != want_desc:
            wrong_desc.append(page)
    if wrong_body:
        problems.append(
            str(len(wrong_body))
            + " llms-full.txt entr(ies) do not carry their page's committed markdown --"
            + " truncated, padded, or holding some other page's body: " + show(wrong_body)
        )
    if wrong_desc:
        problems.append(
            str(len(wrong_desc))
            + " llms-full.txt entr(ies) carry a description that is not the one in the"
            + " page's front matter: " + show(wrong_desc)
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
# Same rule as the bundle, and for the same three attacks: a twin IS
# its page's committed markdown with the front matter stripped, so that
# is what it is compared to. A length floor let a padded twin through
# and could not tell two swapped twins apart at all.
wrong_twin = [
    u
    for u in published
    if twin_for(u) in twins
    and u in source
    and (site / twin_for(u)).read_text().strip() != source[u][1]
]
if wrong_twin:
    problems.append(
        str(len(wrong_twin))
        + " markdown twin(s) are not their page's committed markdown -- present,"
        + " counted, and holding something else: " + show(wrong_twin)
    )

# --- the CUE lexer, read off the rendered page rather than the registry.
#
# The expectation is derived twice over and stated nowhere: which fences
# are CUE comes from each page's COMMITTED SOURCE, and what "CUE was
# highlighted" means comes from the block the build produced for it. So
# this covers every cue fence the site has today and every one added
# tomorrow -- there is no list of pages and no count here to keep in
# step. Reading the fences out of the twin (which is what this did
# first) made the twin the authority on how much there was to check: a
# hook that stripped fenced blocks from the twins took 28 of 31 fences
# out of scope and left this pass reporting OK.
FENCE = re.compile(r"^(?P<prefix>[ \t>]*)(?P<fence>`{3,}|~{3,})(?P<info>.*)$")
BLOCK = re.compile(r"<div class=\"highlight\"><pre>(.*?)</pre></div>", re.S)
SPAN = re.compile(r"<span class=\"([^\"]*)\">(.*?)</span>", re.S)
# pygments' short names for the two token families that separate CUE
# from the plausible impostors. `Comment.*` is the `c` family;
# `Name.Class` is `nc`.
COMMENT_CLASSES = {"c", "c1", "cm", "cp", "cpf", "cs", "ch"}
CLASS_NAME_CLASS = "nc"


def squash(text):
    """A whitespace-free signature, so a source fence can be matched to its block."""
    return re.sub(r"\s+", "", text)


def language(info):
    """The fence's language, across the spellings MkDocs renders as one.

    ```` ```cue ````, ``~~~cue`` and pymdownx's own attribute form
    ```` ```{.cue} ```` all reach the same lexer, so all three have to
    be visible here. They were not: the delimiter run was backticks
    only, and `{.cue}` was read as a language literally called
    `{.cue}`. Both spellings rendered as highlighted CUE on the page and
    neither was checked. (`cli/docsgen/src/scan.rs::fence_open` already
    accepted `~`; two scanners in one gate disagreeing about what a
    fence is, is its own defect.)
    """
    text = info.strip()
    if text.startswith("{"):
        text = text[1:].partition("}")[0].strip()
        if text.startswith("."):
            text = text[1:]
    first = text.split()
    return first[0].lower() if first else ""


def fences(markdown):
    """(language, body) per fenced block. Handles blockquoted and indented fences."""
    out = []
    lang = None
    delimiter = ""
    width = 0
    quoted = False
    body = []
    for line in markdown.split("\n"):
        hit = FENCE.match(line)
        # A backtick fence's info string may not contain a backtick
        # (CommonMark); a tilde fence's may. Without this a line of
        # prose backticks would open a fence.
        if hit and hit.group("fence")[0] == "`" and "`" in hit.group("info"):
            hit = None
        if lang is None:
            if hit:
                lang = language(hit.group("info"))
                delimiter = hit.group("fence")[0]
                width = len(hit.group("fence"))
                quoted = ">" in hit.group("prefix")
                body = []
            continue
        closing = (
            hit
            and hit.group("fence")[0] == delimiter
            and len(hit.group("fence")) >= width
            and not hit.group("info").strip()
        )
        if closing:
            out.append((lang, "\n".join(body)))
            lang = None
            continue
        if quoted:
            line = re.sub(r"^[ \t]*> ?", "", line)
        body.append(line)
    return out


def code_blocks(page_html):
    """(signature, [(css class, text)]) per highlighted block, in document order."""
    return [
        (squash(visible(inner)), [(cls, txt) for cls, txt in SPAN.findall(inner)])
        for inner in BLOCK.findall(page_html)
    ]


def token_starting(spans, classes, prefix):
    """Did any span in `classes` hold text starting with `prefix`?"""
    return any(
        set(cls.split()) & classes and visible(txt).startswith(prefix) for cls, txt in spans
    )


checked = 0
proven = 0
unhighlighted = []
unrendered = []
not_cue = []
for url in published:
    if url not in source:
        continue
    page_html = (site / url / "index.html").read_text()
    blocks = code_blocks(page_html)
    for lang, body in fences(source[url][1]):
        if lang != "cue":
            continue
        checked += 1
        # `all`, not `any`: two fences on one page can share a body (a
        # licence header written once as CUE and once as Rust), and the
        # weaker test would let the styled twin vouch for the plain one.
        hits = [spans for signature, spans in blocks if signature == squash(body)]
        if not hits:
            unrendered.append(url)
            continue
        if not all(spans for spans in hits):
            unhighlighted.append(url)
            continue
        # "Some lexer took" is not the claim worth making: a hook
        # listed after cue_lexer.py can put `class CUE(YamlLexer)` in
        # `_lexer_cache["CUE"]` and every block still comes back full of
        # spans, with `//` not a comment and `#Application` a comment
        # rather than a definition. So where the SOURCE says CUE, the
        # rendered block is required to have read it as CUE:
        #
        #   a `//` line  -> a comment token that starts with `//`
        #                   (YAML has no `//` comment; it makes that a
        #                   scalar)
        #   a `#Name`    -> a Name.Class token that starts with `#`
        #                   (YAML makes that a comment, JSON and text
        #                   make it nothing)
        #
        # A future refinement of CueLexer keeps both; a different
        # language cannot.
        wants_comment = any(line.lstrip().startswith("//") for line in body.split("\n"))
        wants_definition = bool(re.search(r"(?<![\w\"'])_?#[A-Za-z_]", body))
        for spans in hits:
            if wants_comment and not token_starting(spans, COMMENT_CLASSES, "//"):
                not_cue.append(url)
            elif wants_definition and not token_starting(spans, {CLASS_NAME_CLASS}, "#"):
                not_cue.append(url)
            elif wants_comment or wants_definition:
                proven += 1

if not checked:
    lexer_problems.append(
        "not one cue fence was found in the committed source of any published page, so"
        " this check proved nothing. Either the corpus stopped writing CUE or `fences`"
        " stopped recognising the spelling it is written in -- a check that cannot fail"
        " is not a check"
    )
elif not proven and not (not_cue or unrendered or unhighlighted):
    # Only when nothing else is wrong: if every fence came back plain,
    # "none of them proved to be CUE" is a restatement, and a report
    # that says the same thing twice is a report people skim.
    lexer_problems.append(
        "every cue fence on the site rendered with syntax tokens, but not one of them"
        " carries a `//` comment or a `#Definition` -- the two constructs that tell CUE"
        " apart from the languages it could silently be lexed as. The identity half of"
        " this check proved nothing; add one to a fence, or read `token_starting` here"
    )
if unrendered:
    lexer_problems.append(
        str(len(unrendered)) + " cue fence(s) have no matching highlighted block on"
        + " their page at all, so pygments is not rendering code blocks here"
        + " (`use_pygments: false`, or a superfences change): " + show(unrendered)
    )
if unhighlighted:
    lexer_problems.append(
        str(len(unhighlighted)) + " cue fence(s) rendered with no syntax tokens --"
        + " they are plain text on the published page: " + show(unhighlighted)
    )
if not_cue:
    lexer_problems.append(
        str(len(not_cue)) + " cue fence(s) rendered with syntax tokens that are not"
        + " CUE's: a `//` line did not come back as a comment, or a `#Definition` did"
        + " not come back as a definition. Something other than this hook's CueLexer"
        + " claimed the `cue` alias: " + show(not_cue)
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
    + " markdown twin(s), each carrying its page's committed markdown and its"
    + " authored description; CUE lexer OK: " + str(checked) + " highlighted fence(s), "
    + str(proven) + " of them proved to be CUE rather than merely coloured"
)
