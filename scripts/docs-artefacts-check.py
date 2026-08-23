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

Four more holes, and the shape they share
-----------------------------------------

All four were "the artefact is right, so the site must be":

* **The published HTML was asserted for nothing but its ``cue``
  blocks.** All three LLM artefacts derive from ``page.markdown``,
  which mkdocs settles BEFORE the page is rendered — so a hook
  returning ``re.sub(r"<p>.*?</p>", "", html)`` from
  ``on_page_content`` published every page with its prose gone and
  left this pass reporting the artefacts intact, because they were.
  The machine-readable layer vouched for a site it had not read.
  ``rendered_text`` closes it from the source side: every published
  page's ``<article>`` must still carry its own H1 and the longest
  runs of its committed prose. Deleting ``admonition`` from
  ``markdown_extensions``, or stripping ``<meta name="description">``
  in ``on_post_page`` — half of this branch's feature — landed green
  the same way, so the meta description is compared here too.
* **A page in ``nav:`` could be unpublished with everything green.**
  ``orphan_pages`` asserts published ⊆ source and nothing asserted the
  other direction, so deleting a nav line and adding the page to
  ``exclude_docs`` removed it from the site, the index, the bundle and
  the twins without a WARNING anywhere. The missing direction is
  asserted against the *drift gate's* scope rule, read out of
  ``cli/docsgen/src/scan.rs``: unpublishing a guide now has to move a
  Rust constant too.
* **``llms-full.txt`` was compared by cardinality.** Equal counts hid a
  dropped page behind a duplicated one — the exact failure the twins
  block had already been fixed for, in a comment forty lines further
  down. It is a set comparison in both directions now, duplicates
  named.
* **The index's shape was unasserted and its label discarded.**
  Reduced to the copyright line plus one flat ``- [Page](url): desc``
  per entry, every structural element ADR 0057 describes as the
  deliverable was gone and this pass still printed OK. The H1, the
  description blockquote, the licence line, the section per nav group
  and each entry's label are checked against ``mkdocs.yml``, the
  literate-nav fragment and the page's own front matter.
"""

import html
import posixpath
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


def squash(text):
    """A whitespace-free signature of some text.

    Used by both readers of rendered HTML below, for one reason: a tag
    boundary becomes a space here (`<[^>]+>` → " "), so `` `$N` `` in a
    heading comes back as `( $N ACL)` where the source wrote `($N ACL)`.
    Comparing squashed text asks "is this content on the page", which is
    the question, rather than "did the renderer put spaces where the
    author did", which is not.
    """
    return re.sub(r"\s+", "", text)


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


# The link rewrite the hook applies to every twin and bundle body,
# RE-DERIVED here rather than imported: a twin sits at a different URL
# from its page, so a relative link written against the source tree
# resolves a directory wrong once moved, and the hook rewrites each to an
# absolute URL. This is that same rule, independently, so the byte
# compare below is source -> expectation, never the hook vouching for
# itself. `url_for`/`twin_for` above are what map a `.md` target to the
# page's twin.
LINK = re.compile(r"(?<=\]\()([^)]+)(?=\))")
FENCE_LINE = re.compile(r"^[ \t]*(?:`{3,}|~{3,})")
# An inline code span: a `](…)` inside one is prose about link syntax
# (an ADR carries `[a-z0-9]([a-z0-9-]*[a-z0-9])?` and `- [Page](url)`),
# not a link, so the rewrite and the scanners below step over it.
CODE_SPAN = re.compile(r"(`+).+?\1")
# Already resolvable as written: an absolute URL (`scheme:`), a
# protocol-relative `//host`, or a bare `#fragment`.
ABSOLUTE = re.compile(r"^(?:[a-z][a-z0-9+.-]*:|//|#)")


def line_links(line):
    """Link targets on a line, outside any inline code span."""
    targets, pos = [], 0
    for span in CODE_SPAN.finditer(line):
        targets.extend(LINK.findall(line[pos : span.start()]))
        pos = span.end()
    targets.extend(LINK.findall(line[pos:]))
    return targets


def absolutise(target, src_uri):
    """One relative link target -> the absolute URL the hook must emit."""
    if ABSOLUTE.match(target):
        return target
    path, hashsep, frag = target.partition("#")
    if not path:
        return target
    resolved = posixpath.normpath(posixpath.join(posixpath.dirname(src_uri), path))
    if resolved.endswith(".md"):
        dest = base + twin_for(url_for(resolved))
    else:
        dest = base + resolved
    return dest + hashsep + frag


def rewrite_line(line, src_uri):
    """Absolutise the links on one line, leaving inline code spans alone."""
    out, pos = [], 0
    for span in CODE_SPAN.finditer(line):
        out.append(LINK.sub(lambda m: absolutise(m.group(0), src_uri), line[pos : span.start()]))
        out.append(span.group(0))
        pos = span.end()
    out.append(LINK.sub(lambda m: absolutise(m.group(0), src_uri), line[pos:]))
    return "".join(out)


def rewrite_links(body, src_uri):
    """The body a twin/bundle entry must carry: relative links absolutised."""
    out = []
    in_fence = False
    for line in body.split("\n"):
        if FENCE_LINE.match(line):
            in_fence = not in_fence
            out.append(line)
        elif in_fence:
            out.append(line)
        else:
            out.append(rewrite_line(line, src_uri))
    return "\n".join(out)


def surviving_relative_links(body):
    """Relative link targets left in a rewritten body, outside code.

    The property `rewrite_links` is meant to establish, checked on the
    RESULT rather than by re-running the rewrite: whatever the transform
    did, a twin or bundle entry read on its own must not carry a link that
    only resolves against the source tree it no longer sits in. Inline
    code spans are stepped over -- a `](…)` there is prose, not a link.
    """
    bad = []
    in_fence = False
    for line in body.split("\n"):
        if FENCE_LINE.match(line):
            in_fence = not in_fence
        elif not in_fence:
            bad.extend(t for t in line_links(line) if not ABSOLUTE.match(t))
    return bad


def served(url):
    """Does the built site actually serve this absolute in-site URL?

    The other half of "every link is absolute": absolute is not enough if
    it 404s. Resolved against the built site rather than against the rule
    that wrote it -- a `.md` link lands on a twin, a `/`-terminated link
    on a directory's `index.html`, anything else on a static file (or the
    page's own `index.html`). A `#fragment`/`?query` is dropped first.
    """
    rel = url[len(base):].split("#")[0].split("?")[0]
    if rel == "":
        return (site / "index.html").is_file()
    if (site / rel).is_file():
        return True
    if rel.endswith("/"):
        return (site / rel / "index.html").is_file()
    return (site / (rel + "/index.html")).is_file()


def dead_site_links(body):
    """In-site absolute links (base-rooted) a twin carries that 404."""
    dead = []
    in_fence = False
    for line in body.split("\n"):
        if FENCE_LINE.match(line):
            in_fence = not in_fence
        elif not in_fence:
            dead.extend(t for t in line_links(line) if t.startswith(base) and not served(t))
    return dead


# A fenced block, delimiter run and blockquote prefix matched on both
# ends. Prose lives outside these; a shell transcript inside one is not
# a sentence the renderer owes the reader.
SOURCE_FENCE = re.compile(r"(?ms)^([ \t>]*)(`{3,}|~{3,}).*?^\1?\2[ \t]*$")
# A run of plain words: letters, digits and spaces only, long enough to
# be a sentence fragment rather than a word. Deliberately narrow — every
# character it admits survives the markdown-to-HTML conversion
# unchanged, so a run found in the source must appear on the page. The
# moment punctuation is admitted the comparison starts depending on the
# renderer's typography rather than on the content.
WORD_RUN = re.compile(r"[A-Za-z][A-Za-z0-9 ]{12,}")


def markdown_prose(body):
    """A page's committed markdown with the parts that are not prose removed."""
    return SOURCE_FENCE.sub(" ", re.sub(r"(?s)<!--.*?-->", " ", body))


def longest_runs(body, count=3):
    """The `count` longest plain-word runs in a page's prose, longest first."""
    found = {re.sub(r"\s+", " ", m.group(0)).strip() for m in WORD_RUN.finditer(markdown_prose(body))}
    return sorted(found, key=len, reverse=True)[:count]


def heading(body):
    """The page's H1 as text, or '' when it has none.

    The `{#anchor}` suffix `attr_list` consumes and the backticks around
    an inline code span are removed, because neither reaches the reader:
    every other character of an H1 in this corpus does.
    """
    for line in markdown_prose(body).split("\n"):
        if line.startswith("# "):
            return re.sub(r"\s+", " ", re.sub(r"\{#[^}]*\}\s*$", "", line[2:]).replace("`", "")).strip()
    return ""


def title_of(src_uri, body):
    """What the page calls itself: its front-matter `title:`, else its H1."""
    return titles.get(src_uri) or heading(body)


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
# {source path: front-matter `title:`}, for the entries in llms.txt that
# take their label from the page rather than from the nav.
titles = {}
for path in tracked:
    if not path.endswith(".md"):
        continue
    src_uri = Path(path).relative_to(docs_dir).as_posix()
    body, front = meta.get_data(Path(path).read_text())
    description = front.get("description")
    title = front.get("title")
    if title:
        titles[src_uri] = " ".join(str(title).split())
    source[url_for(src_uri)] = (
        src_uri,
        body.strip(),
        " ".join(str(description).split()) if description else "",
    )

# Everything under docs/ is published on THIS site, so a link to a page's
# GitHub blob view is a second canonical address for a page that already
# has one here -- and the off-site one is the address that gets cited as
# the source. mkdocs cannot catch it: `validation.links.absolute_links`
# fires on links to this site, and a github.com link is legitimately
# off-site as written. Scanned on the committed source, where the link is
# authored, so it fails before the build rather than after publication.
GITHUB_DOCS = re.compile(r"github\.com/apprafter/apprafter/blob/[^)\s]*/docs/")
github_docs_links = sorted(
    src_uri for src_uri, body, _ in source.values() if GITHUB_DOCS.search(body)
)
if github_docs_links:
    problems.append(
        str(len(github_docs_links))
        + " page(s) link to a `github.com/apprafter/apprafter/blob/*/docs/**` URL -- a"
        + " second, off-site address for a page this site already publishes; link the"
        + " site-relative `.md` path instead: " + show(github_docs_links)
    )

# What the site actually published, as site-root-relative URL paths.
# Deriving the page SET from the build output is the whole point:
# a page added tomorrow is covered without editing this script.
published = sorted(
    "" if p.parent == site else p.parent.relative_to(site).as_posix() + "/"
    for p in site.rglob("index.html")
    if not p.relative_to(site).as_posix().startswith("assets/")
)
# The numbered ADR records are bundled and twinned but deliberately not
# indexed -- docs/hooks/llm_export.py says why. `adr/README.md` (the ADR
# index page, published at `adr/`) is NOT a record and IS indexed, so it
# is the `ADRs` nav group's one representative on the map. Matched here
# on the URL rather than the source path, and independently of the hook:
# a numbered record's URL is `adr/NNNN-…/`, the index page's is `adr/`.
ADR_RECORD = re.compile(r"adr/\d")
indexable = [u for u in published if not ADR_RECORD.match(u)]
indexable_set = set(indexable)

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

# The OTHER direction, which `orphan_pages` is not: that check asserts
# published <= source, and nothing asserted source <= published. Deleting
# a page's line from `nav:` and adding it to `exclude_docs:` therefore
# unpublished it with a clean --strict build, no ERROR, no WARNING, and
# every count in this file balanced -- the page is simply absent from
# the site, the index, the bundle and the twins alike. That is
# `docs/operator-guide/node-prep.md`, the page ADR 0057 cites as the
# orphan nobody noticed for months, and mkdocs.yml says in as many words
# that `exclude_docs` is for internal artefacts rather than for pages.
#
# The expectation comes from the OTHER TOOL in this gate: the drift
# gate's corpus, whose scope rule is `git ls-files -- docs README.md`
# minus a short list of directory prefixes. That list is READ from the
# Rust rather than restated here, so unpublishing a guide has to move a
# constant in `cli/docsgen/src/scan.rs` -- a reviewable line in the
# checker -- as well as two lines of mkdocs.yml. `README.md` needs no
# exemption: it is not under `docs/`, so it never entered `source`.
def gate_scope_prefixes():
    """The directory prefixes `docsgen`'s corpus excludes, read from it."""
    scan_rs = Path("cli/docsgen/src/scan.rs").read_text()
    render_rs = Path("cli/docsgen/src/render.rs").read_text()
    listed = re.search(r"const EXCLUDED: \[&str; \d+\] = \[(.*?)\];", scan_rs, re.S)
    generated = re.search(r'pub const DIR: &str = "([^"]+)";', render_rs)
    if not listed or not generated:
        return None
    # `is_in_scope` spells the generated tree as `DIR` + '/' rather than
    # listing it, so that renaming the output directory moves the
    # exclusion with it. Same here, and for the same reason.
    return re.findall(r'"([^"]+)"', listed.group(1)) + [generated.group(1) + "/"]


gate_prefixes = gate_scope_prefixes()
if gate_prefixes is None:
    problems.append(
        "the drift gate's scope rule could not be read out of cli/docsgen/src/scan.rs"
        " and cli/docsgen/src/render.rs, so this pass cannot tell which committed pages"
        " are supposed to be on the site. Either `EXCLUDED` / `DIR` were reshaped, in"
        " which case update `gate_scope_prefixes` here, or the files moved"
    )
else:
    gated = sorted(
        url
        for url, (src_uri, _, _) in source.items()
        if not any(f"{docs_dir}/{src_uri}".startswith(prefix) for prefix in gate_prefixes)
    )
    unpublished = [url for url in gated if url not in set(published)]
    if unpublished:
        problems.append(
            str(len(unpublished))
            + " committed page(s) inside the drift gate's corpus were not published at"
            + " all, so they are absent from the site, from llms.txt, from"
            + " llms-full.txt and from the twins -- and mkdocs reports an"
            + " `exclude_docs:` entry that nobody links as nothing whatsoever: "
            + show(
                source[url][0] for url in unpublished
            )
        )

def one_line(text):
    """Collapse a config value to one line, or '' when absent."""
    return " ".join(str(text).split()) if text else ""


def summary_entries(directory):
    """(label, source path) per entry of a `literate-nav` SUMMARY.md fragment.

    `mkdocs.yml` points at `reference/cli/` and literate-nav expands it
    from that directory's SUMMARY.md, so the labels of every generated
    page are authored THERE and nowhere else. Reading the fragment is
    what lets the index's `CLI / apprafter app` entries be checked
    against a committed label rather than merely against non-emptiness.
    """
    fragment = docs_dir / directory / "SUMMARY.md"
    if not fragment.is_file():
        return None
    return [
        (" ".join(label.split()), directory + target)
        for label, target in re.findall(
            r"(?m)^\s*[-*+]\s*\[([^\]]*)\]\(([^)\s]+)\)", fragment.read_text()
        )
    ]


# Nav items this pass does not understand. Reported rather than skipped:
# a shape that falls through would silently take pages out of every
# expectation below, which is the failure mode this whole file is about.
nav_unread = []


def nav_leaves(items, prefix):
    """[(label, source path)] under a nav subtree, sub-section labels joined by ' / '.

    Mirrors what `_ordered` in the hook builds: a page's entry label is
    the nav's own label for it, qualified by any sub-section between it
    and the top-level group -- which is why `reference/index.md` and
    `reference/cli/index.md` do not both read 'Overview'.
    """
    out = []
    for item in items:
        if not isinstance(item, dict) or len(item) != 1:
            nav_unread.append(repr(item))
            continue
        ((label, value),) = item.items()
        if isinstance(value, list):
            out.extend(nav_leaves(value, prefix + (label,)))
        elif isinstance(value, str) and value.endswith("/"):
            found = summary_entries(value)
            if found is None:
                nav_unread.append(value)
                continue
            out.extend(
                (" / ".join(prefix + (label, sub)), src_uri) for sub, src_uri in found
            )
        elif isinstance(value, str) and value.endswith(".md"):
            out.append((" / ".join(prefix + (label,)), value))
        # Anything else is an external link, which indexes nothing.
    return out


# (heading, [(entry label, source path), ...]) per top-level nav group,
# in nav order -- the sections `llms.txt` is organised by.
nav_groups = []
for item in config["nav"] or []:
    if not isinstance(item, dict) or len(item) != 1:
        nav_unread.append(repr(item))
        continue
    ((group, value),) = item.items()
    # A top-level page is its own group of one, exactly as the hook
    # treats it: it is a tab in the rendered nav just as a section is.
    nav_groups.append((group, nav_leaves(value if isinstance(value, list) else [item], ())))
nav_label = {src_uri: label for _, leaves in nav_groups for label, src_uri in leaves}
if nav_unread:
    problems.append(
        str(len(nav_unread))
        + " nav entr(ies) in mkdocs.yml are in a shape this pass cannot read, so the"
        + " pages under them are checked against nothing: " + show(nav_unread)
    )

# Restated here rather than read out of docs/hooks/llm_export.py, and
# deliberately: a heading read from the hook is the hook vouching for
# its own output. This is the label the index must carry for the pages
# that are published, in scope and off the nav.
UNSECTIONED = "Other pages"
# Per-page label overrides that the hook applies to avoid a nav-title
# echo (e.g. `## ADRs` heading + `[ADRs]` entry both reading "ADRs").
# Must stay in sync with the equivalent dict in llm_export.py's
# `_ordered()`.  Keyed by src_uri.
LABEL_OVERRIDES: dict[str, str] = {
    "adr/README.md": "ADR index",
}

index_path = site / "llms.txt"
index = ""
if not index_path.is_file():
    problems.append("llms.txt was not written")
else:
    index = index_path.read_text()
    # The index's SHAPE, not just its entries. Reduced to a licence line
    # and one flat `- [Page](url): description` per page, everything ADR
    # 0057 calls the deliverable was gone -- no H1, no site description,
    # no section per nav group, no generated twin-rule sentence -- and
    # the only surviving guard was a `CC-BY-4.0` substring. Each
    # expectation below comes from mkdocs.yml, from the literate-nav
    # fragment or from the built site; none from the artefact.
    lines = index.split("\n")
    want_title = "# " + config["site_name"]
    if not lines or lines[0].strip() != want_title:
        problems.append(
            "llms.txt does not open on the site's H1 (`" + want_title + "` from"
            " mkdocs.yml's `site_name:`); the convention's first line is that heading"
        )
    want_blurb = "> " + one_line(config["site_description"])
    if config["site_description"] and want_blurb not in lines:
        problems.append(
            "llms.txt carries no site-description blockquote (`" + want_blurb + "`,"
            " from mkdocs.yml's `site_description:`)"
        )
    want_licence = one_line(config["copyright"])
    if want_licence and want_licence not in one_line(index):
        problems.append(
            "llms.txt does not carry the licence line mkdocs.yml's `copyright:`"
            " declares, so the index states different terms from the site footer: "
            + want_licence
        )
    # The sentence the hook GENERATES rather than writes out, checked
    # against the things it describes: the twin rule against `twin_for`
    # (which the twins block below enforces against the site), and the two
    # bundle URLs plus the two counts against the pages the build
    # published. An index naming a twin rule, a bundle or a count the site
    # does not back is a detectable lie, and it was not detected.
    if base:
        root_twin = base + twin_for("")
        guides_url = base + "llms-guides.txt"
        bundle_url = base + "llms-full.txt"
        stated = [
            line
            for line in lines
            if root_twin in line and guides_url in line and bundle_url in line
        ]
        if not stated:
            problems.append(
                "llms.txt does not, in one line, name where a page's markdown twin"
                " lives, where the indexed pages are bundled and where the whole corpus"
                " is: no line names all of `" + root_twin + "` (the site root's twin,"
                " per `twin_for`), `" + guides_url + "` and `" + bundle_url + "`. A"
                " reader of the index cannot discover them from anywhere else"
            )
        else:
            # The numbered records only -- the ADR index page IS listed,
            # so the "N numbered records under adr/" count excludes it
            # (matches the hook's `n_excluded`); the indexed count is every
            # published page the map lists (matches the hook's `n_indexed`).
            n_records = sum(1 for u in published if ADR_RECORD.match(u))
            n_indexed = len(indexable)
            claimed_records = re.search(r"(\d+) numbered records under", stated[0])
            claimed_indexed = re.search(r"(\d+) indexed pages", stated[0])
            if not claimed_records or not claimed_indexed:
                problems.append(
                    "llms.txt's generated sentence no longer states both counts (the"
                    " indexed-page total bundled in llms-guides.txt and the"
                    " numbered-record total it leaves out), so a number that could be"
                    " wrong is now absent instead: " + stated[0].strip()[:160]
                )
            else:
                if int(claimed_records.group(1)) != n_records:
                    problems.append(
                        "llms.txt says it leaves " + claimed_records.group(1)
                        + " numbered record(s) out of the index; the build published "
                        + str(n_records) + " under `adr/`"
                    )
                if int(claimed_indexed.group(1)) != n_indexed:
                    problems.append(
                        "llms.txt says it bundles " + claimed_indexed.group(1)
                        + " indexed page(s) in llms-guides.txt; the build published "
                        + str(n_indexed) + " indexable page(s)"
                    )
    headings = re.findall(r"(?m)^## (.+)$", index)
    want_headings = [
        group
        for group, leaves in nav_groups
        if any(url_for(src_uri) in indexable_set for _, src_uri in leaves)
    ]
    if any(source[u][0] not in nav_label for u in indexable):
        want_headings.append(UNSECTIONED)
    absent = [h for h in want_headings if h not in headings]
    if absent:
        problems.append(
            str(len(absent))
            + " nav group(s) have no `## ` section in llms.txt, so the index is a flat"
            + " list where the convention asks for a map: " + show(absent)
        )
    stray = [h for h in headings if h not in want_headings]
    if stray:
        problems.append(
            str(len(stray)) + " `## ` section(s) in llms.txt name no top-level nav group"
            + " in mkdocs.yml (and are not `" + UNSECTIONED + "`): " + show(stray)
        )
    kept = [h for h in headings if h in want_headings]
    if kept != [h for h in want_headings if h in headings]:
        problems.append(
            "llms.txt's sections are not in nav order -- mkdocs.yml reads "
            + ", ".join(want_headings)
            + " and the index reads " + ", ".join(headings)
        )

# The label and the description are captured, not just the URL: an
# earlier version matched `^- \[[^\]]*\]\((\S+)\)` and so could not see
# an entry line that had dropped its `: description` suffix. Every
# authored description would have been gone from the index with this
# pass reporting OK. The suffix is OPTIONAL in the pattern so that its
# absence is reported as the missing description it is, rather than as
# a page missing from the index.
ENTRY = re.compile(r"(?m)^- \[(?P<label>[^\]]*)\]\((?P<url>[^)\s]+)\)(?::[ \t]*(?P<desc>.*))?$")
entries = [(m.group("label"), m.group("url"), m.group("desc")) for m in ENTRY.finditer(index)]
urls = [url for _, url, _ in entries]
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
    # The same split, asserted the other way. `unlisted` requires every
    # non-ADR page to be listed and nothing required the ADRs to stay
    # out, so the judgement `docs/hooks/llm_export.py` argues for --
    # index the guides, bundle the records -- was only half checked.
    listed_records = sorted(u for u in listed if u in set(published) - indexable_set)
    if listed_records:
        problems.append(
            str(len(listed_records))
            + " page(s) held out of the index on purpose (`adr/`, which is about half"
            + " of everything the site publishes) are listed in llms.txt after all, so"
            + " the map buries the guides it exists to point at: " + show(listed_records)
        )
    # The index's ONE authored input, checked against where it was
    # authored. `_require_descriptions` in the hook fails the build when
    # a page's front matter has no description; nothing asserted that
    # the description reached the artefact, and the entry line can drop
    # it while every count above still balances.
    #
    # The LABEL is checked beside it, and against the same kind of
    # source: the nav's own label for the page where the nav names one
    # (mkdocs.yml, or the literate-nav fragment for the generated
    # pages), and the page's own `title:`/H1 where it does not. Only the
    # URL and the description were checked before, so an index whose
    # every entry read `- [Page](…)` passed -- and a label is what a
    # reader picks an entry by.
    undescribed = []
    mismatched = []
    unlabelled = []
    relabelled = []
    for label, url, desc in entries:
        if not url.startswith(base):
            continue
        page = url[len(base):]
        if page not in source:
            continue
        src_uri, body, authored = source[page]
        want_label = LABEL_OVERRIDES.get(src_uri) or nav_label.get(src_uri) or title_of(src_uri, body)
        if not label.strip():
            unlabelled.append(page)
        elif want_label and label.strip() != want_label:
            relabelled.append(page + " (`" + label.strip() + "` for `" + want_label + "`)")
        if not (desc or "").strip():
            undescribed.append(page)
        elif desc.strip() != authored:
            mismatched.append(page)
    if unlabelled:
        problems.append(
            str(len(unlabelled))
            + " llms.txt entr(ies) carry no label at all: " + show(unlabelled)
        )
    if relabelled:
        problems.append(
            str(len(relabelled))
            + " llms.txt entr(ies) are labelled with something that is neither the nav's"
            + " label for the page nor the page's own title: " + show(relabelled)
        )
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

# One bundle entry (also one twin, which is a one-entry bundle):
# `audience:` is [^\n]+ rather than \S+ so a two-word audience does not
# read as a missing page.
BUNDLE_ENTRY = re.compile(
    r"(?ms)^---\nurl: (\S+)\n(description: [^\n]*\n)?audience: [^\n]+\n---\n(.*?)"
    r"(?=^---\nurl: |\Z)"
)


def check_bundle(name, expected_pages):
    """A bundle artefact vs the exact pages it should carry, byte for byte.

    The same three attacks the twins block guards: a SET comparison both
    ways with repeats named (a dropped page hides behind a duplicated one
    when only counts are compared -- removing the largest operator guide
    and appending a second `license.md` left the old check balanced and
    the corpus a page short), and each entry's body and description
    compared to the committed source -- not "is it long enough", which
    filler passes and swapped bodies defeat. The body is compared to the
    source with the link rewrite RE-DERIVED here, and the result is then
    checked for any surviving relative link, so a rewrite that silently
    stopped absolutising is caught from the other side too.
    """
    path = site / name
    if not path.is_file():
        problems.append(name + " was not written")
        return
    entries = BUNDLE_ENTRY.findall(path.read_text())
    urls = [url for url, _, _ in entries]
    repeated = sorted({u for u in urls if urls.count(u) > 1})
    if repeated:
        problems.append(
            str(len(repeated)) + " page(s) appear in " + name + " more than once, which"
            + " is also how a missing page hides from a count: " + show(repeated)
        )
    if not base:
        return
    expected = set(expected_pages)
    pages = {u[len(base):] for u in urls if u.startswith(base)}
    absent = sorted(expected - pages)
    if absent:
        problems.append(
            str(len(absent)) + " page(s) that belong in " + name + " are missing, so the"
            + " corpus it hands a model is not the set it should be: " + show(absent)
        )
    extra = sorted(pages - expected)
    if extra:
        problems.append(
            str(len(extra)) + " " + name + " entr(ies) name a page that does not belong"
            + " in this bundle: " + show(extra)
        )
    wrong_body, wrong_desc, stray_links = [], [], []
    for url, desc_line, body in entries:
        if not url.startswith(base):
            continue
        page = url[len(base):]
        if page not in source:
            continue
        src_uri, want_body, want_desc = source[page]
        if body.strip() != rewrite_links(want_body, src_uri):
            wrong_body.append(page)
        got_desc = desc_line[len("description: "):].strip() if desc_line else ""
        if got_desc != want_desc:
            wrong_desc.append(page)
        if surviving_relative_links(body):
            stray_links.append(page)
    if wrong_body:
        problems.append(
            str(len(wrong_body)) + " " + name + " entr(ies) do not carry their page's"
            + " committed markdown with links absolutised -- truncated, padded, wrongly"
            + " rewritten, or holding another page's body: " + show(wrong_body)
        )
    if wrong_desc:
        problems.append(
            str(len(wrong_desc)) + " " + name + " entr(ies) carry a description that is"
            + " not the one in the page's front matter: " + show(wrong_desc)
        )
    if stray_links:
        problems.append(
            str(len(stray_links)) + " " + name + " entr(ies) still carry a relative link"
            + " that only resolves against the source tree, not from where the bundle is"
            + " served: " + show(stray_links)
        )


# llms-full.txt is the whole corpus; llms-guides.txt is the indexed
# (non-record) pages only -- the same set llms.txt maps.
check_bundle("llms-full.txt", published)
check_bundle("llms-guides.txt", indexable)

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
# Same rule as the bundle, and for the same attacks: a twin IS its
# page's committed markdown with the front matter stripped, behind the
# SAME derived header a bundle entry carries (so a twin fetched on its
# own still says where it came from) and with its relative links
# absolutised (so they resolve from the twin's URL, not the source
# tree). Each is compared to the source, header re-derived, links
# re-derived; a length floor let a padded twin through and could not tell
# two swapped twins apart at all.
no_header, wrong_twin, twin_wrong_desc, twin_stray_links, twin_dead = [], [], [], [], []
for u in published:
    if twin_for(u) not in twins or u not in source:
        continue
    src_uri, want_body, want_desc = source[u]
    entry = BUNDLE_ENTRY.match((site / twin_for(u)).read_text())
    if not entry:
        no_header.append(u)
        continue
    got_url, desc_line, body = entry.group(1), entry.group(2), entry.group(3)
    got_desc = desc_line[len("description: "):].strip() if desc_line else ""
    if got_url != base + u or got_desc != want_desc:
        twin_wrong_desc.append(u)
    if body.strip() != rewrite_links(want_body, src_uri):
        wrong_twin.append(u)
    if surviving_relative_links(body):
        twin_stray_links.append(u)
    if dead_site_links(body):
        twin_dead.append(u + " (" + dead_site_links(body)[0][len(base):] + ")")
if no_header:
    problems.append(
        str(len(no_header)) + " markdown twin(s) do not open on the derived `---` header"
        + " (url / [description] / audience) that a page fetched as markdown on its own"
        + " needs to be self-describing: " + show(no_header)
    )
if twin_wrong_desc:
    problems.append(
        str(len(twin_wrong_desc)) + " markdown twin(s) carry a header whose url or"
        + " description is not the one derived from the page: " + show(twin_wrong_desc)
    )
if wrong_twin:
    problems.append(
        str(len(wrong_twin))
        + " markdown twin(s) are not their page's committed markdown (links absolutised)"
        + " -- present, counted, and holding something else: " + show(wrong_twin)
    )
if twin_stray_links:
    problems.append(
        str(len(twin_stray_links)) + " markdown twin(s) still carry a relative link that"
        + " only resolves against the source tree, not from the twin's URL: "
        + show(twin_stray_links)
    )
if twin_dead:
    problems.append(
        str(len(twin_dead)) + " markdown twin(s) carry an absolute in-site link the built"
        + " site does not serve -- absolutised to a URL that 404s: " + show(twin_dead)
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


# A rendered mermaid diagram. Material's superfences mapping turns a
# ```mermaid fence into `<pre class="mermaid">` (not a pygments
# `<div class="highlight">`) and loads mermaid.js to typeset it. So a
# mermaid fence needs its own signature set: if the `custom_fences`
# mapping is dropped from mkdocs.yml, superfences renders the fence as an
# ordinary code listing instead and the diagram silently becomes a wall
# of `flowchart` source -- a green build with no diagram on the page.
MERMAID_BLOCK = re.compile(r'<pre class="mermaid">(.*?)</pre>', re.S)


def mermaid_blocks(page_html):
    """Signatures of the mermaid diagrams a page rendered."""
    return {squash(visible(inner)) for inner in MERMAID_BLOCK.findall(page_html)}


def token_starting(spans, classes, prefix):
    """Did any span in `classes` hold text starting with `prefix`?"""
    return any(
        set(cls.split()) & classes and visible(txt).startswith(prefix) for cls, txt in spans
    )


# --- what the reader was actually served.
#
# Everything above this line reads an artefact that derives from
# `page.markdown`, which mkdocs settles BEFORE the page is rendered. So
# all of it can be intact on a site that publishes nothing: a hook
# returning `re.sub(r"<p>.*?</p>", "", html)` from `on_page_content`
# published every page with all of its paragraphs gone, and this pass
# reported the twins, the bundle and the index all carrying their pages'
# committed markdown, because they did. The machine-readable layer
# vouched for a site it had not read.
#
# So the rendered page is asserted against the SAME committed source,
# and not by length: a page's `<article>` must still carry its own H1
# and the longest runs of plain prose the source writes. Deleting
# `admonition` from `markdown_extensions` (which publishes
# `!!! warning "…"` as literal text) and any other break in the
# markdown-to-HTML path lands here for the same reason. No count is
# recorded -- the runs come out of whatever the page says today.
# The other half of that path, which the prose runs do NOT reach.
# Deleting `admonition` from `markdown_extensions` publishes
# `!!! warning "This guide provisions a paid server"` as a paragraph of
# literal text -- every word of the source is still on the page, so a
# content comparison passes it. What changes is that a line the source
# wrote as SYNTAX was not consumed, and that is what is asserted: an
# opener for a container the configured extensions own must not survive
# verbatim into the rendered text. One line covers `admonition`,
# `pymdownx.details`, `pymdownx.tabbed`, `pymdownx.tasklist`, `tables`
# and `pymdownx.snippets`. It is a spot-check of the markdown-to-HTML
# path rather than a per-extension audit -- `attr_list` needs none,
# because a lost `{#anchor}` is a dead anchor and `--strict` already
# fails pass 1 on those.
#
# One shape would read here as a false accusation: a four-space
# INDENTED code block holding one of these lines renders as code, so the
# line is on the page verbatim and legitimately. The corpus has no
# indented code blocks at all (`cli/docsgen/tests/scan_test.rs`'s
# `corpus_census` prints the count, and `docsgen gate` reports any block
# that renders as code without a fence), and if one ever arrives the
# report names the page and the line, which is the right place to look
# either way.
BLOCK_SYNTAX = re.compile(r'^(?:!!! |\?\?\?\+? |=== "|--8<--|[-*+] \[[ xX]\] |\|.*\|$)')
ARTICLE = re.compile(r"<article[^>]*>(.*?)</article>", re.S)
META = re.compile(r'<meta name="description" content="([^"]*)"')
# The per-page pointer at the markdown twin (overrides/main.html). It is
# what makes the whole export path reachable by an agent that lands on an
# HTML page rather than guessing `/llms.txt`; without it the twins,
# llms.txt and the bundles are invisible from the rendered site. The href
# must be exactly where the twin is served, so it is compared to
# `twin_for(url)` -- the same rule the twins block enforces against disk.
ALT = re.compile(r'<link rel="alternate" type="text/markdown" href="([^"]*)"')

checked = 0
proven = 0
unhighlighted = []
unrendered = []
not_cue = []
# Fences on pages the committed census does not floor -- see the OK line.
unfloored = 0
no_article = []
no_heading = []
lost_prose = []
raw_syntax = []
lost_block = []
lost_mermaid = []
wrong_meta = []
wrong_alt = []
for url in published:
    if url not in source:
        continue
    page_html = (site / url / "index.html").read_text()
    _, page_md, authored = source[url]

    rendered = ARTICLE.search(page_html)
    if not rendered:
        no_article.append(url)
    else:
        text = squash(visible(rendered.group(1)))
        title = heading(page_md)
        if not title:
            no_heading.append(url)
        elif squash(title) not in text:
            lost_prose.append(url)
        else:
            runs = longest_runs(page_md)
            if not runs:
                # A published page whose source holds no run of plain
                # words at all is either not prose or not a page; either
                # way there is nothing here to assert, and saying so is
                # better than passing silently.
                no_heading.append(url)
            elif any(squash(run) not in text for run in runs):
                lost_prose.append(url)
        leaked = [
            line.strip()
            for line in markdown_prose(page_md).split("\n")
            if BLOCK_SYNTAX.match(line.strip()) and squash(line.strip()) in text
        ]
        if leaked:
            raw_syntax.append(url + " (`" + leaked[0][:60] + "`)")

    # The other half of the front-matter `description:` this subphase
    # added, and the half nothing checked: it becomes the page's entry in
    # llms.txt AND the page's own HTML meta description. An `on_post_page`
    # hook deleting the tag took the site from a description per page to
    # none with every artefact assertion above still green. A page with
    # no authored description gets `site_description` from the theme, so
    # that is what it is held to -- mkdocs.yml either way, never the tag
    # itself.
    want_meta = authored or one_line(config["site_description"])
    got_meta = META.search(page_html)
    if want_meta and (not got_meta or one_line(html.unescape(got_meta.group(1))) != want_meta):
        wrong_meta.append(url)

    if base:
        want_alt = base + twin_for(url)
        got_alt = ALT.search(page_html)
        if not got_alt or got_alt.group(1) != want_alt:
            wrong_alt.append(url)

    blocks = code_blocks(page_html)
    page_fences = fences(page_md)
    signatures = {signature for signature, _ in blocks}
    mermaid_sigs = mermaid_blocks(page_html)
    # EVERY fenced block, not only the `cue` ones. The prose assertion
    # above cannot cover these: `markdown_prose` deletes fenced blocks
    # before `longest_runs` reads the source, deliberately -- a shell
    # transcript is not a sentence -- so without this the largest
    # content surface on the site is unasserted. The corpus is 169 `sh`
    # fences against 17 in-scope `cue` ones, and an `on_page_content`
    # hook that stripped code blocks from every page holding no ```cue
    # fence removed 185 of 358 published blocks across 50 pages with the
    # whole gate green, `operator-guide/troubleshooting` going from 2 to
    # 0. Source -> artefact, so a stripped, truncated or swapped block
    # all fail it; every one of the corpus's fences matches today, so
    # this ships with no exemption.
    for lang, body in page_fences:
        # A mermaid fence renders as a diagram, not a code block, so it is
        # asserted against the mermaid blocks the page produced -- a
        # POSITIVE check that enabling mermaid actually took, and stays.
        target_sigs = mermaid_sigs if lang == "mermaid" else signatures
        if squash(body) not in target_sigs:
            first = next((line for line in body.split("\n") if line.strip()), "")
            marker = url + " (```" + (lang or "") + " `" + first.strip()[:50] + "`)"
            (lost_mermaid if lang == "mermaid" else lost_block).append(marker)
    # `docs/adr/` is out of the drift gate's corpus, so the committed
    # census's `cue_fences` floor does not reach these -- reported on the
    # OK line rather than left to be discovered.
    unfloored += sum(1 for lang, _ in page_fences if lang == "cue" and url.startswith("adr/"))
    for lang, body in page_fences:
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

if no_article:
    problems.append(
        str(len(no_article)) + " published page(s) have no `<article>` element at all,"
        + " so the theme rendered no content region and nothing about what a reader"
        + " sees can be judged here: " + show(no_article)
    )
if lost_block:
    problems.append(
        str(len(lost_block)) + " fenced block(s) in a page's committed source do not appear"
        + " among the code blocks that page rendered -- stripped, truncated or replaced"
        + " somewhere between the markdown and the published HTML, with the LLM artefacts"
        + " (which are built from the source, before rendering) still vouching for them: "
        + show(lost_block)
    )
if lost_mermaid:
    problems.append(
        str(len(lost_mermaid)) + " ```mermaid fence(s) did not render as a"
        + " `<pre class=\"mermaid\">` diagram -- the superfences `custom_fences` mermaid"
        + " mapping is gone from mkdocs.yml, so the diagram is published as a wall of"
        + " source instead: " + show(lost_mermaid)
    )
if no_heading:
    problems.append(
        str(len(no_heading)) + " published page(s) have no H1 and no run of plain prose"
        + " in their committed source, so this pass has nothing to require of the"
        + " rendered page: " + show(no_heading)
    )
if lost_prose:
    problems.append(
        str(len(lost_prose)) + " published page(s) render without their own heading or"
        + " their own prose: the committed markdown is in the twin and in the bundle,"
        + " and it is NOT on the page. Something between markdown and HTML is dropping"
        + " content -- an `on_page_content` hook, a removed `markdown_extensions` entry,"
        + " a theme override: " + show(lost_prose)
    )
if raw_syntax:
    problems.append(
        str(len(raw_syntax)) + " published page(s) render a line the source wrote as"
        + " block SYNTAX -- an admonition, a tab, a table row, a task item, a snippet"
        + " include -- as literal text, so the extension that owns it is no longer"
        + " configured or no longer runs: " + show(raw_syntax)
    )
if wrong_meta:
    problems.append(
        str(len(wrong_meta)) + " published page(s) carry no `<meta name=\"description\">`"
        + " or carry one that is neither the page's authored description nor mkdocs.yml's"
        + " `site_description:` -- half of what a page's front-matter `description:` is"
        + " for: " + show(wrong_meta)
    )
if wrong_alt:
    problems.append(
        str(len(wrong_alt)) + " published page(s) carry no `<link rel=\"alternate\""
        + " type=\"text/markdown\">`, or one whose href is not where the twin is served"
        + " (base + twin_for(url)) -- so an agent on the HTML page cannot find the markdown"
        + " layer, and overrides/main.html is missing or has drifted from `_twin_uri`: "
        + show(wrong_alt)
    )

# robots.txt is the other half of "the export path is reachable": a
# static file copied to the site root that points a crawler at the
# sitemap. The Sitemap line is compared to `site_url` so a change to the
# hostname that left robots.txt behind fails here rather than pointing
# crawlers at the old origin.
if base:
    robots = site / "robots.txt"
    if not robots.is_file():
        problems.append(
            "robots.txt was not published at the site root, so there is no `Sitemap:`"
            " line and no crawl allowance -- add docs/robots.txt (mkdocs copies it)"
        )
    else:
        want_sitemap = "Sitemap: " + base + "sitemap.xml"
        if want_sitemap not in robots.read_text():
            problems.append(
                "robots.txt carries no `" + want_sitemap + "` line, so it does not point a"
                " crawler at the sitemap mkdocs writes at that URL"
            )

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
    + str(len(published)) + " bundled in llms-full.txt, " + str(len(indexable))
    + " in llms-guides.txt, " + str(len(twins))
    + " markdown twin(s), each behind a derived header, carrying its page's committed"
    + " markdown with links absolutised and its authored description; "
    + str(len(published)) + " page(s) rendered carrying"
    + " their own heading, prose and meta description; CUE lexer OK: " + str(checked)
    + " highlighted fence(s), " + str(proven) + " of them proved to be CUE rather than"
    + " merely coloured, " + str(unfloored) + " of them on `adr/` pages the committed"
    + " census does not floor"
)
