# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""Publish the documentation in a form an LLM can read without scraping HTML.

Three artefacts, all written into ``config.site_dir`` by
:func:`on_post_build` and **none of them committed**:

``llms.txt``
    The curated index, in the shape the convention asks for: an ``H1``
    with the site name, a blockquote with the site description, the
    licence line, then one ``##`` section per top-level nav group whose
    entries are ``- [label](absolute url): description``.

``llms-full.txt``
    Every published page's markdown in one file, each preceded by a
    header carrying ``url``, ``description`` and ``audience``.

a markdown twin per page
    ``<page>.md`` beside ``<page>/index.html``, holding the page's
    source markdown with the front matter stripped.

Nothing here is committed because nothing here is authored: all three
derive from committed content at build time, so they cannot drift from
it.  The one authored input is the ``description:`` line in each page's
front matter, and :func:`_index_entries` **fails the build** when an
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

- **excluded from ``llms.txt``.**  The ADRs are historical records —
  each describes the world as it was on the day it was ratified, which
  is why the drift gate holds them out of scope too.  Indexing them
  would also bury the guides: they are half of everything the site
  publishes (see the counts below).
- **included in ``llms-full.txt``**, and in the markdown twins.  They
  are the densest available explanation of *why* the system is shaped
  the way it is — which is exactly what is wanted when the question is
  a design question rather than a how-do-I question.  A model handed
  the full corpus should have them; a model handed a map should not
  have to walk past them.

Re-derive the shape of the corpus with::

    nix develop --command mkdocs build --strict --site-dir /tmp/e-llm -q
    grep -c '^- \\[' /tmp/e-llm/llms.txt            # indexed pages
    grep -c '^url: ' /tmp/e-llm/llms-full.txt       # bundled pages
    find /tmp/e-llm -name '*.md' | wc -l            # markdown twins

At 2026-08-19 those read 59, 118 and 118: 118 published pages, of which
59 are under ``adr/`` and so appear in the bundle and as twins but not
in the index.  ``scripts/docs-check.sh`` asserts all three counts
against the built site on every run, so they are checked rather than
recorded.

One thing this does not do
--------------------------

``pymdownx.snippets`` include lines (``--8<--``) are resolved during the
markdown-to-HTML conversion, not before it, so a twin carries the
include line rather than the included file.  That is currently
academic — ``git grep -c -- '--8<--' -- docs`` reports no matches, the
extension is configured and available but unused — and it is recorded
here rather than pre-solved because the fix (running the real
``snippet`` preprocessor over the source) is a handful of lines to add
against a real use site, and guessing at it now would be untested
machinery of exactly the kind ADR 0057 set out to stop accumulating.
"""

from __future__ import annotations

import posixpath
import re
from dataclasses import dataclass
from pathlib import Path

from mkdocs.exceptions import PluginError
from mkdocs.structure.nav import Page as NavPage
from mkdocs.structure.nav import Section

# Source-path prefixes held out of `llms.txt` only. See the module
# docstring for why this one is a judgement rather than a mirror of
# `exclude_docs`: these pages ARE published, ARE bundled and DO get a
# twin — they are just not on the map.
_INDEX_EXCLUDED_PREFIXES = ("adr/",)

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
    """Collapse a front-matter value to one line, or '' when absent."""
    return " ".join(str(text).split()) if text else ""


def _twin_uri(dest_uri: str) -> str:
    """Where the markdown twin goes, relative to ``site_dir``.

    The rule is "beside the rendered page", not "mirror the source
    path": with ``use_directory_urls`` (the default here) mkdocs renders
    ``dev-guide/quickstart.md`` to ``dev-guide/quickstart/index.html``,
    and the twin belongs at ``dev-guide/quickstart.md`` — one URL, one
    trailing ``/`` swapped for ``.md``.  A section index page
    (``architecture/index.md`` → ``architecture/index.html``) therefore
    lands at ``architecture.md``, which is again the page URL with its
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
        f"it does not end in '.html', so it is not a rendered page. Either mkdocs "
        f"changed how it names page output or this hook is being handed something "
        f"that is not a page."
    )


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
    nested section, so the 27 generated CLI pages are grandchildren of
    ``Reference``, not children.  They belong under the ``Reference``
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

    This is how a page outside the nav still gets a sensible audience:
    ``contributing/README.md`` is ``not_in_nav``, but its directory
    holds three pages that are in the *Contributing* group, so it
    inherits ``contributing``.  Likewise every ADR record inherits from
    ``adr/README.md``, which is the nav's *ADRs* entry — so the records
    and their index agree instead of splitting into 'adr' and 'adrs'.

    The site root is excluded on purpose: it is not a section. A
    root-level page outside the nav (``license.md``) inherits nothing
    and falls back to :data:`_DEFAULT_AUDIENCE`, rather than claiming
    the audience of the home page it merely sits beside.
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


def _ordered() -> tuple[list[tuple[str, list[tuple[str, _Doc]]]], list[_Doc]]:
    """Split the captured pages into the index's sections and the bundle's order.

    Returns ``(sections, bundle)`` where ``sections`` is
    ``[(heading, [(label, doc), …]), …]`` in nav order with the
    unsectioned pages last, and ``bundle`` is every published page: the
    indexed ones in index order, then the excluded ones (the ADRs)
    sorted by source path so the bundle is byte-stable across builds.
    """
    sections: list[tuple[str, list[tuple[str, _Doc]]]] = []
    seen: set[str] = set()

    for item, pages in _groups:
        entries: list[tuple[str, _Doc]] = []
        for prefix, page in pages:
            src_uri = page.file.src_uri
            seen.add(src_uri)
            doc = _docs.get(src_uri)
            if doc is None or src_uri.startswith(_INDEX_EXCLUDED_PREFIXES):
                continue
            # The nav's own label for the page ('Overview'), which is
            # what a reader picked from the sidebar, qualified by any
            # sub-section it sits in ('CLI / Overview'). A page title
            # would repeat the heading it sits under; a bare nav label
            # is ambiguous the moment two sub-sections use one word.
            entries.append((" / ".join((*prefix, page.title or doc.title)), doc))
        if entries:
            sections.append((_group_label(item), entries))

    unsectioned = [
        doc
        for src_uri, doc in sorted(_docs.items())
        if src_uri not in seen and not src_uri.startswith(_INDEX_EXCLUDED_PREFIXES)
    ]
    if unsectioned:
        sections.append((_UNSECTIONED_LABEL, [(doc.title, doc) for doc in unsectioned]))

    indexed = [doc for _, entries in sections for _, doc in entries]
    indexed_uris = {doc.src_uri for doc in indexed}
    rest = [doc for src_uri, doc in sorted(_docs.items()) if src_uri not in indexed_uris]
    return sections, indexed + rest


def _index_entries(sections) -> None:
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


def _render_index(config, sections, n_excluded: int) -> str:
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
    # Two things a reader of this file cannot otherwise discover: that
    # each page has a markdown twin, and that the ADRs exist at all.
    # Both sentences are generated rather than written, so neither can
    # name a count or a URL the build did not just produce.
    lines += [
        f"Every page below is also published as markdown, at its own URL with the "
        f"trailing `/` dropped and `.md` appended ({site_url}index.md for the site "
        f"root itself). The whole corpus — including the {n_excluded} pages under "
        f"`adr/` that this index does not list — is bundled at "
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


def _render_bundle(config, bundle, nav_audience, dir_audience) -> str:
    """Every page's markdown, each behind a front-matter-shaped header.

    The header repeats the shape the page itself uses, which is the
    least surprising thing to hand a reader that has just been handed a
    corpus of markdown.  ``description`` is omitted rather than invented
    where a page has none — the ADRs carry no front matter at all, and a
    fabricated one-line summary of a historical record is precisely the
    unchecked claim this track exists to keep out.
    """
    site_url = config["site_url"]
    out: list[str] = []
    for doc in bundle:
        out.append("---")
        out.append(f"url: {site_url}{doc.url}")
        if doc.description:
            out.append(f"description: {doc.description}")
        out.append(f"audience: {_audience(doc, nav_audience, dir_audience)}")
        out.append("---")
        out.append("")
        out.append(doc.markdown.strip())
        out.append("")
    return "\n".join(out)


def on_config(config):
    """Drop anything held over from a previous build (`mkdocs serve` rebuilds)."""
    _docs.clear()
    _groups.clear()
    return config


def on_nav(nav, config, files):
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


def on_page_content(html, page, config, files):
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
        description=_one_line(page.meta.get("description")),
        meta_audience=_one_line(page.meta.get("audience")),
        markdown=page.markdown or "",
    )
    return html


def on_post_build(config):
    """Write the three artefacts into the built site."""
    if not _docs:
        raise PluginError(
            "docs/hooks/llm_export.py captured no pages, so llms.txt, llms-full.txt "
            "and the markdown twins would all be empty or absent. The build cannot "
            "have rendered a page without calling on_page_content — check that this "
            "hook is still listed under `hooks:` in mkdocs.yml and that mkdocs still "
            "fires that event."
        )

    site_dir = Path(config["site_dir"])
    sections, bundle = _ordered()
    _index_entries(sections)

    nav_audience = {
        page.file.src_uri: _slug(_group_label(item))
        for item, pages in _groups
        for _, page in pages
    }
    dir_audience = _dir_audiences()
    n_excluded = sum(1 for uri in _docs if uri.startswith(_INDEX_EXCLUDED_PREFIXES))

    (site_dir / "llms.txt").write_text(_render_index(config, sections, n_excluded), "utf-8")
    (site_dir / "llms-full.txt").write_text(
        _render_bundle(config, bundle, nav_audience, dir_audience), "utf-8"
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
        target.write_text(doc.markdown.strip() + "\n", "utf-8")
