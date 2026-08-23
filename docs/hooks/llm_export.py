# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""Publish the documentation in a form an LLM can read without scraping HTML.

Four artefacts, all written into ``config.site_dir`` by
:func:`on_post_build` and **none of them committed**:

``llms.txt``
    The curated index, in the shape the convention asks for: an ``H1``
    with the site name, a blockquote with the site description, the
    licence line, then one ``##`` section per top-level nav group whose
    entries are ``- [label](absolute url): description``.

``llms-guides.txt``
    The indexed pages' markdown in one file — the same set ``llms.txt``
    maps (guides + the ADR index, minus the numbered records) — each
    behind the same header ``llms-full.txt`` uses. The corpus a model
    should read when the question is a how-do-I one, without the ADR
    records it does not need.

``llms-full.txt``
    Every published page's markdown in one file (guides *and* the ADR
    records), each preceded by a header carrying ``url``, ``description``
    and ``audience``.

a markdown twin per page
    ``<page>.md`` beside ``<page>/index.html``, holding the page's
    source markdown — front matter stripped, relative links absolutised —
    behind that same header.

Nothing here is committed because nothing here is authored: all four
derive from committed content at build time, so they cannot drift from
it.  The one authored input is the ``description:`` line in each page's
front matter, and :func:`_require_descriptions` **fails the build** when an
indexed page lacks one — the index is worthless without it, and a
silently-absent description is the failure mode this whole gate exists
to remove.

What is in scope
----------------

The page set is whatever mkdocs actually built.  ``exclude_docs`` in
``mkdocs.yml`` (the superpowers tree, the changelog working files, the
measurements tree, the literate-nav fragment, these hooks) already
removes those pages from the ``Files`` collection, so mkdocs never
calls :func:`on_page_content` for them and they are absent here for
free.  That is deliberate: restating the exclusion list would make this
file a second source of truth for it, and the second one is the one
that goes stale.

``docs/adr/**`` is the one judgement call, and it is split:

- **the numbered records are excluded from ``llms.txt``.**  Each
  ``adr/NNNN-…`` describes the world as it was on the day it was
  ratified — historical records, which is why the drift gate holds them
  out of scope too — and indexing all fifty-odd would bury the guides
  they sit beside.  ``adr/README.md`` is the exception: it is the ADR
  *index*, not a record, and the only ``adr/`` page authored WITH a
  ``description:``, so that the ``ADRs`` nav group is represented on the
  map by its one overview page rather than not at all.
  :func:`_excluded_from_index` draws that line — a numbered record is
  off the map, the index page is on it.
- **every ADR — records and index alike — is included in
  ``llms-full.txt``**, and in the markdown twins.  They are the densest
  available explanation of *why* the system is shaped the way it is —
  which is exactly what is wanted when the question is a design question
  rather than a how-do-I question.  A model handed the full corpus
  should have them; a model handed a map should get only the one page
  that maps them.

Re-derive the shape of the corpus with::

    nix develop --command mkdocs build --strict --site-dir /tmp/e-llm -q
    grep -c '^- \\[' /tmp/e-llm/llms.txt            # indexed pages
    grep -c '^url: ' /tmp/e-llm/llms-full.txt       # bundled pages
    find /tmp/e-llm -name '*.md' | wc -l            # markdown twins

No numbers are recorded beside those commands, deliberately.  A count
written next to the command that produces it is the one thing in this
file that goes stale on its own — a page added to the nav moves all
three.  What holds regardless is the *relation*: the bundle and the
twins cover every published page, and the index covers that set minus
the numbered ``adr/`` records (the ADR index page stays in).
``scripts/docs-check.sh`` asserts exactly those relations
against the site on every run, so they are checked rather than written
down.

It asserts the *content* too, and against the committed source rather
than against anything this hook produced: each twin and each bundle
entry must **be** its page's markdown with the front matter stripped,
and each index entry must carry the description that page's front
matter authored.  :func:`_require_descriptions` below fails the build
when a page has no description, but nothing in this file can attest
that the description reached ``llms.txt`` — an entry line rendered
without its ``: description`` suffix leaves every count in the artefact
balanced.  That is why the check lives on the other side.

One thing this does not do
--------------------------

``pymdownx.snippets`` include lines (``--8<--``) are resolved during the
markdown-to-HTML conversion, not before it, so a twin carries the
include line rather than the included file.  That is currently
academic: no published page uses one.  Re-derive with::

    git grep -c -- '--8<--' -- docs ':!docs/hooks' ':!docs/changelog'

(no matches).  The two exclusions are what make that a measurement of
the *corpus* rather than of this paragraph: this file mentions the
marker while explaining it, and the plan ledger quotes it while
recording the explanation — neither is a page, and both would otherwise
show up as the very use sites the sentence denies.  The extension is
configured and available but unused, and this is recorded rather than
pre-solved because the fix (running the real ``snippet`` preprocessor
over the source) is a handful of lines to add against a real use site,
and guessing at it now would be untested machinery of exactly the kind
ADR 0057 set out to stop accumulating.
"""

from __future__ import annotations

import posixpath
import re
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

from mkdocs.config.defaults import MkDocsConfig
from mkdocs.exceptions import PluginError
from mkdocs.structure.files import Files
from mkdocs.structure.nav import Navigation
from mkdocs.structure.nav import Page as NavPage
from mkdocs.structure.nav import Section

# The numbered ADR records held out of `llms.txt` only. See the module
# docstring for why this is a judgement rather than a mirror of
# `exclude_docs`: these pages ARE published, ARE bundled and DO get a
# twin — they are just not on the map. The predicate matches a numbered
# record (`adr/0001-…`, and the `adr/0000-template` slot) but NOT
# `adr/README.md`: that page is the ADR index, authored WITH a
# `description:` to be indexed, and is the only representation the
# `ADRs` nav group would otherwise have on the map.
_INDEX_EXCLUDED_RECORD = re.compile(r"adr/\d")


def _excluded_from_index(src_uri: str) -> bool:
    """A numbered ADR record — bundled and twinned, but kept off the map."""
    return bool(_INDEX_EXCLUDED_RECORD.match(src_uri))

# The label for pages that are published and in scope but sit outside
# the nav (`not_in_nav` in mkdocs.yml lists them deliberately). Without
# this the index would silently omit a page a reader can reach by URL.
_UNSECTIONED_LABEL = "Other pages"

# Audience for a page that inherits from nothing — a page at the site
# root outside the nav. The root is not a section, so it is not a
# fallback either.
_DEFAULT_AUDIENCE = "general"


@dataclass
class _Doc:
    """One published page, captured as mkdocs rendered it."""

    src_uri: str  # 'dev-guide/quickstart.md'
    url: str  # 'dev-guide/quickstart/' — site-root-relative
    twin_uri: str  # 'dev-guide/quickstart.md' — inside site_dir
    title: str
    description: str  # '' when the page carries none
    meta_audience: str  # '' when the page carries none
    markdown: str


# One index section: its heading, and (label, page) in nav order.
_Section = tuple[str, list[tuple[str, "_Doc"]]]


# Rebuilt from scratch on every `on_config`, because `mkdocs serve`
# reuses this module across rebuilds and stale entries would resurrect
# a page that was deleted.
_docs: dict[str, _Doc] = {}
# (top-level nav item, [(titles of the sections between it and the
# page, page), …]). The intermediate titles are kept because the nav
# nests: without them `reference/index.md` and `reference/cli/index.md`
# are both an entry called "Overview" under the same heading. The nav
# ITEM is kept rather than its title, because a nav entry with no
# explicit title takes one from the page's first heading, which is not
# resolved until the page has been rendered.
_groups: list[tuple[object, list[tuple[tuple[str, ...], NavPage]]]] = []


def _slug(text: str) -> str:
    """Lowercase, non-alphanumerics to single hyphens: 'Public ingress' → 'public-ingress'."""
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def _one_line(text: object) -> str:
    """Collapse a value to one line, or '' when absent."""
    return " ".join(str(text).split()) if text else ""


def _authored_line(value: object, src_uri: str, field: str) -> str:
    """The same, for an AUTHORED front-matter field — and typed.

    Every other malformed shape of ``description:`` fails loudly with
    the page named: absent fails in :func:`_require_descriptions`, empty
    is absent, and a multi-line block is collapsed to one line.  A wrong
    *type* was the hole.  YAML infers, so ``description: [a, b]`` is a
    list, ``description: {a: b}`` is a map and ``description: 3`` is an
    int — and ``str()`` renders all three into something that reaches
    ``llms.txt`` as the page's summary reading ``['a', 'b']``, with
    nothing anywhere reporting it.  A summary is prose or it is a
    defect, so a non-string is a build failure.
    """
    if value is None:
        return ""
    if not isinstance(value, str):
        raise PluginError(
            f"docs/hooks/llm_export.py: {src_uri!r} has a `{field}:` that YAML read "
            f"as {type(value).__name__}, not as text: {value!r}. It would reach "
            "llms.txt as this page's summary in exactly that shape. Write it as one "
            "quoted sentence."
        )
    return _one_line(value)


def _twin_uri(dest_uri: str) -> str:
    """Where the markdown twin goes, relative to ``site_dir``.

    The rule is "beside the rendered page", not "mirror the source
    path": with ``use_directory_urls`` (the default here) mkdocs renders
    ``dev-guide/quickstart.md`` to ``dev-guide/quickstart/index.html``,
    and the twin belongs at ``dev-guide/quickstart.md`` — one URL, one
    trailing ``/`` swapped for ``.md``.  A section index page
    (``reference/index.md`` → ``reference/index.html``) therefore
    lands at ``reference.md``, which is again the page URL with its
    slash replaced.  The site root is the one page with no slash to
    replace, so it keeps ``index.md``.
    """
    if dest_uri == "index.html":
        return "index.md"
    if dest_uri.endswith("/index.html"):
        return dest_uri[: -len("/index.html")] + ".md"
    # `use_directory_urls: false` — 'dev-guide/quickstart.html'.
    if dest_uri.endswith(".html"):
        return dest_uri[: -len(".html")] + ".md"
    raise PluginError(
        f"docs/hooks/llm_export.py cannot place a markdown twin for {dest_uri!r}: "
        "it does not end in '.html', so it is not a rendered page. Either mkdocs "
        "changed how it names page output or this hook is being handed something "
        "that is not a page."
    )


def _url_for(src_uri: str) -> str:
    """The site URL mkdocs gives a source page, from its path alone.

    The source-path counterpart of :func:`_twin_uri` (which works off the
    rendered *dest* path): ``index.md`` and ``README.md`` are a
    directory's index page, everything else gets a directory of its own.
    It exists so :func:`_rewrite_links` can turn a page's own relative
    links — written against the source tree — into the absolute site URLs
    the twins and the bundle need.
    """
    directory, _, name = src_uri.rpartition("/")
    if name[: -len(".md")] in ("index", "README"):
        return directory + "/" if directory else ""
    return src_uri[: -len(".md")] + "/"


def _twin_for(url: str) -> str:
    """Where the twin of a page with this URL is served, URL-relative.

    The same mapping as :func:`_twin_uri`, keyed by the URL rather than
    the rendered dest path, so a link *target* resolved to a page URL can
    be pointed at that page's twin.
    """
    return "index.md" if url == "" else url.rstrip("/") + ".md"


# The `(target)` of an inline markdown link or image — the text between
# `](` and the closing `)`. Targets in this corpus carry no spaces or
# nested parentheses, so `[^)]+` is safe; a target that did would need
# the `<…>` form and would not match, which is the safe direction.
_LINK = re.compile(r"(?<=\]\()([^)]+)(?=\))")
# An inline code span (a backtick run, then text, then the same run).
# Rewriting inside one would corrupt a documented pattern — an ADR
# carries `[a-z0-9]([a-z0-9-]*[a-z0-9])?` and another `- [Page](url)`,
# both of which contain `](…)` that is prose about syntax, not a link.
_CODE_SPAN = re.compile(r"(`+).+?\1")
# A fenced-code delimiter. Links are rewritten OUTSIDE fences AND inline
# code only: a code sample that happened to contain link syntax must
# survive verbatim into the twin and the bundle.
# `scripts/docs-artefacts-check.py` re-derives this rewrite and
# byte-compares, so a divergence fails the build.
_FENCE = re.compile(r"^[ \t]*(?:`{3,}|~{3,})")
# A target that is already resolvable as written: an absolute URL (any
# `scheme:`), a protocol-relative `//host`, or a bare `#fragment`.
_ABSOLUTE = re.compile(r"^(?:[a-z][a-z0-9+.-]*:|//|#)")


def _absolutise(target: str, src_uri: str, site_url: str) -> str:
    """One relative link target → an absolute site URL; others unchanged.

    A ``.md`` target is another page, and is pointed at that page's
    markdown TWIN — a reader that followed a ``.md`` link asked for plain
    text and should keep getting it — resolved through the same
    source→url→twin rule the build uses. Any other relative target (an
    asset such as ``cli/commands.json``) is pointed at the path the site
    serves it from. Absolute URLs, protocol-relative URLs and pure
    ``#fragment`` links are already resolvable and are left alone.
    """
    if _ABSOLUTE.match(target):
        return target
    path, hashsep, frag = target.partition("#")
    if not path:
        return target
    resolved = posixpath.normpath(posixpath.join(posixpath.dirname(src_uri), path))
    if resolved.endswith(".md"):
        dest = site_url + _twin_for(_url_for(resolved))
    else:
        dest = site_url + resolved
    return dest + hashsep + frag


def _rewrite_line(line: str, src_uri: str, site_url: str) -> str:
    """Absolutise the links on one line, leaving inline code spans alone."""
    out: list[str] = []
    pos = 0
    for span in _CODE_SPAN.finditer(line):
        out.append(
            _LINK.sub(lambda m: _absolutise(m.group(0), src_uri, site_url), line[pos : span.start()])
        )
        out.append(span.group(0))
        pos = span.end()
    out.append(_LINK.sub(lambda m: _absolutise(m.group(0), src_uri, site_url), line[pos:]))
    return "".join(out)


def _rewrite_links(markdown: str, src_uri: str, site_url: str) -> str:
    """Rewrite a page's relative links to absolute, outside code.

    A twin and a bundle entry carry a page's markdown at a DIFFERENT URL
    from the page itself (``<dir>/index.md`` renders at ``<dir>/`` but its
    twin lands at ``<dir>.md``), so a relative link written against the
    source tree resolves against the wrong base once moved. The section
    index pages were the worst case: every ``quickstart.md`` on one
    resolved a directory too high and 404'd. Absolute URLs do not move.
    Fenced blocks and inline code spans are left untouched — a ``](…)``
    there is prose about link syntax, not a link.
    """
    out: list[str] = []
    in_fence = False
    for line in markdown.split("\n"):
        if _FENCE.match(line):
            in_fence = not in_fence
            out.append(line)
        elif in_fence:
            out.append(line)
        else:
            out.append(_rewrite_line(line, src_uri, site_url))
    return "\n".join(out)


def _header(config: MkDocsConfig, doc: "_Doc", audience: str) -> list[str]:
    """The derived front-matter header the bundle AND the twins carry.

    Not the page's authored front matter — the *derived* view: an
    absolute ``url``, the authored ``description`` where the page has one,
    and the ``audience`` resolved from the nav. Emitting it on the twin
    too (it used to reach only ``llms-full.txt``) means a model handed a
    single page's markdown in isolation — the twin is the artefact
    ``llms.txt`` advertises per page — can still say where it came from
    and who it is for.
    """
    lines = ["---", f"url: {config['site_url']}{doc.url}"]
    if doc.description:
        lines.append(f"description: {doc.description}")
    lines.append(f"audience: {audience}")
    lines.append("---")
    return lines


def _group_label(item: object) -> str:
    """The heading a top-level nav item contributes to the index.

    Read at post-build time, never at ``on_nav`` time: a ``Section``
    always has its title from the configuration, but a top-level
    ``Page`` may be taking its title from its own first heading, which
    is set when the page renders.
    """
    title = getattr(item, "title", None)
    if title:
        return _one_line(title)
    return getattr(getattr(item, "file", None), "src_uri", "") or "Untitled"


def _section_pages(
    section: Section, prefix: tuple[str, ...] = ()
) -> list[tuple[tuple[str, ...], NavPage]]:
    """Every page under a nav section, at any depth, in nav order.

    Depth matters: `literate-nav` expands ``reference/cli/`` into a
    nested section, so every generated CLI page is a grandchild of
    ``Reference`` rather than a child.  They belong under the ``Reference``
    heading all the same — the index has one level of section, matching
    the top-level nav groups a reader sees as tabs — but the nesting is
    carried out as a label prefix rather than dropped: flattened
    outright, the *Reference* section listed two different pages as
    "Overview".
    """
    out: list[tuple[tuple[str, ...], NavPage]] = []
    for item in section.children:
        if isinstance(item, Section):
            out.extend(_section_pages(item, prefix + (item.title,)))
        elif isinstance(item, NavPage):
            out.append((prefix, item))
    return out


def _dir_audiences() -> dict[str, str]:
    """Map a source directory to the audience of the nav group it sits in.

    This is how a page outside the nav still gets a sensible audience.
    ``not_in_nav`` in ``mkdocs.yml`` holds exactly one pattern —
    ``adr/*`` — so the ADR records are what this map is for today: each
    inherits from ``adr/README.md``, which is the nav's *ADRs* entry, and
    records and index therefore agree on ``adrs`` instead of splitting
    into 'adr' and 'adrs'.  It generalises to any future ``not_in_nav``
    page whose directory has a nav-listed sibling; do not read a
    directory here as evidence that some page in it is out of the nav.

    The site root is excluded on purpose: it is not a section, so a
    root-level page outside the nav would inherit nothing and fall back
    to :data:`_DEFAULT_AUDIENCE` rather than claim the audience of the
    home page it merely sits beside.  No page is in that position today
    (``license.md`` sits at the root but IS in the nav, under
    *Contributing*); the branch is a guard, not a description.
    """
    mapping: dict[str, str] = {}
    for item, pages in _groups:
        for _, page in pages:
            directory = posixpath.dirname(page.file.src_uri)
            if directory:
                mapping.setdefault(directory, _slug(_group_label(item)))
    return mapping


def _audience(doc: _Doc, nav_audience: dict[str, str], dir_audience: dict[str, str]) -> str:
    """Front matter wins; then the nav group; then an ancestor directory's group.

    ``audience`` is optional where ``description`` is required because
    it can be *derived*: every generated page already declares
    ``reference`` and would go on declaring it, and taking the rest from
    the nav gets the same answer with nothing to keep in step.  A field
    an author must retype to state what the tree already says is a field
    that will one day disagree with the tree.
    """
    if doc.meta_audience:
        return doc.meta_audience
    if doc.src_uri in nav_audience:
        return nav_audience[doc.src_uri]
    directory = posixpath.dirname(doc.src_uri)
    while directory:
        if directory in dir_audience:
            return dir_audience[directory]
        directory = posixpath.dirname(directory)
    return _DEFAULT_AUDIENCE


def _ordered() -> tuple[list[_Section], list[_Doc]]:
    """Split the captured pages into the index's sections and the bundle's order.

    Returns ``(sections, bundle)`` where ``sections`` is
    ``[(heading, [(label, doc), …]), …]`` in nav order with the
    unsectioned pages last, and ``bundle`` is every published page: the
    indexed ones in index order, then the excluded ones (the ADRs)
    sorted by source path so the bundle is byte-stable across builds.
    """
    sections: list[_Section] = []
    seen: set[str] = set()

    for item, pages in _groups:
        entries: list[tuple[str, _Doc]] = []
        for prefix, page in pages:
            src_uri = page.file.src_uri
            seen.add(src_uri)
            doc = _docs.get(src_uri)
            if doc is None or _excluded_from_index(src_uri):
                continue
            # The nav's own label for the page ('Overview'), which is
            # what a reader picked from the sidebar, qualified by any
            # sub-section it sits in ('CLI / Overview'). A page title
            # would repeat the heading it sits under; a bare nav label
            # is ambiguous the moment two sub-sections use one word.
            # The ADRs nav item is a top-level Page, so its section
            # heading (`_group_label` below) and this entry label are
            # both the nav title "ADRs" — the index would list "ADRs"
            # twice, once as the `## ADRs` heading and once as its lone
            # entry. Relabel just the entry so the reader sees the
            # heading is the group and the entry is the index page under
            # it. Keyed on the source path, not on a heading==label
            # collision, so the identical `## Home`→[Home] pairing (a
            # deliberate one-page group) is left untouched.
            label = (
                "ADR index"
                if src_uri == "adr/README.md"
                else " / ".join((*prefix, page.title or doc.title))
            )
            entries.append((label, doc))
        if entries:
            sections.append((_group_label(item), entries))

    unsectioned = [
        doc
        for src_uri, doc in sorted(_docs.items())
        if src_uri not in seen and not _excluded_from_index(src_uri)
    ]
    if unsectioned:
        sections.append((_UNSECTIONED_LABEL, [(doc.title, doc) for doc in unsectioned]))

    indexed = [doc for _, entries in sections for _, doc in entries]
    indexed_uris = {doc.src_uri for doc in indexed}
    rest = [doc for src_uri, doc in sorted(_docs.items()) if src_uri not in indexed_uris]
    return sections, indexed + rest


def _require_descriptions(sections: list[_Section]) -> None:
    """Fail the build when an indexed page has no description.

    A description is authored content and cannot be derived: falling
    back to a page's first paragraph yields an index whose entries are
    scene-setting clauses, and — worse — it would be silent, so nobody
    would ever learn the index had gone thin.

    It is also the one front-matter field worth *requiring*, on the test
    2.19d applied to ``verified-by`` and this subphase applied again: a
    description is a summary, not a claim about the world, so an author
    writing one cannot make it false the way a per-page version or a
    test binding goes false while nobody is looking.
    """
    missing = [doc.src_uri for _, entries in sections for _, doc in entries if not doc.description]
    if missing:
        raise PluginError(
            "docs/hooks/llm_export.py: "
            + f"{len(missing)} indexed page(s) carry no `description:` in their front "
            + "matter, so llms.txt would list them with no summary: "
            + ", ".join(sorted(missing))
            + ". Add a `description:` line saying what the page answers: it becomes "
            + "the page's entry in the index AND its HTML meta description, and it "
            + "is the one thing neither can be derived from the tree."
        )


def _render_index(config: MkDocsConfig, sections: list[_Section], n_excluded: int) -> str:
    """The curated map: H1, blockquote, licence, then a section per nav group."""
    site_url = config["site_url"]
    lines = [f"# {config['site_name']}", ""]
    if config["site_description"]:
        lines += [f"> {_one_line(config['site_description'])}", ""]
    # The licence line is read from `copyright:` in mkdocs.yml rather
    # than written out here, so the site footer and this file cannot
    # disagree about the terms. ADR 0057 named them: prose CC-BY-4.0,
    # code samples Apache-2.0.
    if config["copyright"]:
        lines += [_one_line(config["copyright"]), ""]
    # Three things a reader of this file cannot otherwise discover: that
    # each page has a markdown twin, that the indexed pages are bundled on
    # their own, and that the ADR records exist at all beyond this map.
    # Every count and URL is generated rather than written, so none can
    # name a total the build did not just produce.
    n_indexed = sum(len(entries) for _, entries in sections)
    lines += [
        "Every page below is also published as markdown, at its own URL with the "
        f"trailing `/` dropped and `.md` appended ({site_url}index.md for the site "
        f"root itself). These {n_indexed} indexed pages are bundled on their own at "
        f"{site_url}llms-guides.txt; the whole corpus — those plus the {n_excluded} "
        "numbered records under `adr/` that this map does not list — is bundled at "
        f"{site_url}llms-full.txt.",
        "",
    ]
    for heading, entries in sections:
        lines.append(f"## {heading}")
        lines.append("")
        for label, doc in entries:
            lines.append(f"- [{label}]({site_url}{doc.url}): {doc.description}")
        lines.append("")
    return "\n".join(lines)


def _render_bundle(
    config: MkDocsConfig,
    bundle: list[_Doc],
    nav_audience: dict[str, str],
    dir_audience: dict[str, str],
) -> str:
    """Every given page's markdown, each behind a derived header.

    The header repeats the shape the page itself uses, which is the
    least surprising thing to hand a reader that has just been handed a
    corpus of markdown.  ``description`` is omitted rather than invented
    where a page has none — the ADRs carry no front matter at all, and a
    fabricated one-line summary of a historical record is precisely the
    unchecked claim this track exists to keep out.  Relative links are
    rewritten to absolute so that a bundle entry cited on its own still
    resolves.

    Takes the page list rather than deriving it, so the same renderer
    produces both ``llms-full.txt`` (the whole corpus) and
    ``llms-guides.txt`` (the indexed pages only).
    """
    site_url = config["site_url"]
    out: list[str] = []
    for doc in bundle:
        out.extend(_header(config, doc, _audience(doc, nav_audience, dir_audience)))
        out.append("")
        out.append(_rewrite_links(doc.markdown.strip(), doc.src_uri, site_url))
        out.append("")
    return "\n".join(out)


# The site footer names the release the docs describe. The version is
# single-sourced from the committed generated CLI reference — the same
# docsgen origin that stamps `reference/cli/index.md` — so the footer
# cannot name a version the reference does not. Read at build time from
# the committed file (always inside docs_dir, so always reachable);
# falls back to a build-stamped date if that line ever moves, rather
# than failing the build over a decorative footer.
_CLI_VERSION_RE = re.compile(r"at \*\*apprafter (v[0-9][0-9A-Za-z.\-]*)\*\*")


def _cli_version(config: MkDocsConfig) -> str:
    ref = Path(config["docs_dir"]) / "reference" / "cli" / "index.md"
    try:
        m = _CLI_VERSION_RE.search(ref.read_text(encoding="utf-8"))
    except OSError:
        m = None
    if m:
        return m.group(1)
    return "built " + datetime.now(timezone.utc).strftime("%Y-%m-%d")


def on_config(config: MkDocsConfig) -> MkDocsConfig:
    """Drop anything held over from a previous build (`mkdocs serve` rebuilds)."""
    _docs.clear()
    _groups.clear()
    config["extra"] = config.get("extra") or {}
    config["extra"]["cli_version"] = _cli_version(config)
    return config


def on_nav(nav: Navigation, config: MkDocsConfig, files: Files) -> Navigation:
    """Capture the nav groups — the sections `llms.txt` is organised by.

    The nav is read here rather than from ``config['nav']`` because by
    this point `literate-nav` has expanded ``reference/cli/`` from its
    ``SUMMARY.md`` fragment; the raw config still holds the unexpanded
    link, and an index built from it would be missing every generated
    CLI page.  Page objects are kept, not their titles: a nav entry
    without an explicit title takes it from the page's first heading,
    which is not resolved until the page is rendered.
    """
    for item in nav.items:
        if isinstance(item, Section):
            pages = _section_pages(item)
            if pages:
                _groups.append((item, pages))
        elif isinstance(item, NavPage):
            # A top-level page is its own group of one: it is a tab in
            # the rendered nav exactly as a section is.
            _groups.append((item, [((), item)]))
        # A `Link` is an external URL with nothing to index.
    return nav


def on_page_content(html: str, page: NavPage, config: MkDocsConfig, files: Files) -> str:
    """Capture each page's markdown at the point it is final.

    ``on_page_content`` rather than ``on_page_markdown``, because the
    latter is the event whose *return value* becomes ``page.markdown``
    (``page.markdown = config.plugins.on_page_markdown(page.markdown,
    …)``): a handler reading its argument sees the text as it stood
    before every handler after it.  ``on_page_content`` fires once that
    chain has settled and the page has rendered, so what is captured is
    what the reader was served.

    Reading it back in :func:`on_post_build` would work too — mkdocs
    1.6.1 does not clear ``page.markdown`` afterwards, checked, not
    assumed — but it would mean holding the ``Files`` collection and
    re-deriving which of its entries were built.  mkdocs populates only
    the pages that survived ``exclude_docs``, so capturing one page at a
    time here *is* that filter, with nothing restated.
    """
    _docs[page.file.src_uri] = _Doc(
        src_uri=page.file.src_uri,
        url=page.url,
        twin_uri=_twin_uri(page.file.dest_uri),
        title=_one_line(page.title) or page.file.src_uri,
        description=_authored_line(
            page.meta.get("description"), page.file.src_uri, "description"
        ),
        meta_audience=_authored_line(page.meta.get("audience"), page.file.src_uri, "audience"),
        markdown=page.markdown or "",
    )
    return html


def on_post_build(config: MkDocsConfig) -> None:
    """Write the four artefacts into the built site."""
    if not _docs:
        raise PluginError(
            "docs/hooks/llm_export.py captured no pages, so llms.txt, llms-guides.txt, "
            "llms-full.txt and the markdown twins would all be empty or absent. The build cannot "
            "have rendered a page without calling on_page_content — check that this "
            "hook is still listed under `hooks:` in mkdocs.yml and that mkdocs still "
            "fires that event."
        )
    # Every URL written below is absolute, and `site_url` is the only
    # place one can come from. Without it mkdocs leaves the value None
    # and `f"{site_url}{doc.url}"` yields the string "None" — the build
    # stays GREEN and publishes an index whose every link, and every
    # generated sentence naming a path, reads `None`. There is no
    # sensible fallback (a relative index is not what the convention
    # asks for), so this is a hard requirement, stated where it bites.
    if not config["site_url"]:
        raise PluginError(
            "docs/hooks/llm_export.py needs `site_url:` in mkdocs.yml: every URL it "
            "writes into llms.txt and llms-full.txt is absolute, and with no site_url "
            "each one would be published as the literal string 'None'. Set site_url, "
            "or remove this hook from `hooks:`."
        )

    site_dir = Path(config["site_dir"])
    sections, bundle = _ordered()
    _require_descriptions(sections)

    nav_audience = {
        page.file.src_uri: _slug(_group_label(item))
        for item, pages in _groups
        for _, page in pages
    }
    dir_audience = _dir_audiences()
    n_excluded = sum(1 for uri in _docs if _excluded_from_index(uri))
    # The indexed pages, in index order — the same set `llms.txt` maps.
    # `llms-guides.txt` is these; `llms-full.txt` is these plus the
    # numbered records. `_ordered` already split them to build `bundle`;
    # re-derive the indexed half here rather than widen its return type.
    indexed = [doc for _, entries in sections for _, doc in entries]

    (site_dir / "llms.txt").write_text(_render_index(config, sections, n_excluded), "utf-8")
    (site_dir / "llms-full.txt").write_text(
        _render_bundle(config, bundle, nav_audience, dir_audience), "utf-8"
    )
    (site_dir / "llms-guides.txt").write_text(
        _render_bundle(config, indexed, nav_audience, dir_audience), "utf-8"
    )

    claimed: dict[str, str] = {}
    for doc in _docs.values():
        # Two pages resolving to one twin would mean the second silently
        # overwrote the first. mkdocs already rejects two sources
        # rendering to one URL, so this can only fire if the mapping in
        # `_twin_uri` stops being one-to-one — say through a future
        # `use_directory_urls` change. Loud beats lost.
        if doc.twin_uri in claimed:
            raise PluginError(
                f"docs/hooks/llm_export.py: {doc.src_uri!r} and "
                f"{claimed[doc.twin_uri]!r} both want the markdown twin "
                f"{doc.twin_uri!r}; one would overwrite the other."
            )
        claimed[doc.twin_uri] = doc.src_uri
        target = site_dir / doc.twin_uri
        target.parent.mkdir(parents=True, exist_ok=True)
        header = "\n".join(_header(config, doc, _audience(doc, nav_audience, dir_audience)))
        body = _rewrite_links(doc.markdown.strip(), doc.src_uri, config["site_url"])
        target.write_text(header + "\n\n" + body + "\n", "utf-8")
