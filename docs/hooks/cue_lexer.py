# SPDX-License-Identifier: FSL-1.1-Apache-2.0
"""A pygments lexer for CUE, registered at build time — and self-checked.

The pygments version ``flake.lock`` pins ships no CUE lexer:
``get_lexer_by_name("cue")`` raises ``ClassNotFound``, and every
``cue`` fence in the corpus renders as unstyled text.  Re-derive both
halves of that with::

    nix develop --command python3 -c "import pygments.lexers as l; l.get_lexer_by_name('cue')"
    git grep -cE '^[ >]*```cue$' -- docs | awk -F: '{n+=$2} END {print n}'
    git grep -cE '^[ >]*```cue$' -- docs ':!docs/changelog' | awk -F: '{n+=$2} END {print n}'
    git grep -cE '^```cue$'      -- docs | awk -F: '{n+=$2} END {print n}'

No counts are recorded beside those commands: the corpus writes more
CUE every subphase, and a number written next to the command that
measures it is the first thing here to rot.  What the four commands
say, and go on saying, is: the alias does not resolve without this
hook; the second count is smaller than the first because
``exclude_docs`` drops the changelog working file, and it is the number
of fences on the built site; and the fourth is smaller again, because
that pattern requires the fence to open its line.  That last gap is why
the ``[ >]*`` prefix is in the pattern at all — fences written inside a
blockquote or indented under a list item are real fences, and an
anchored count silently loses exactly them.

The measurement that settles it independently of any of these is the
built site: ``scripts/docs-check.sh`` matches every ``cue`` fence in a
page's **committed source** to the block the build produced for it, and
requires that block to have been read *as CUE* — a ``//`` line coming
back as a comment token, a ``#Definition`` as a definition. Both halves
matter. The fence set comes from the committed source rather than from
the markdown twin because the twin is itself an artefact of the other
hook: reading it there let a twin writer that stripped fenced blocks
take 28 of 31 fences out of scope with the pass still reporting OK. And
"carries syntax tokens" was too weak on its own — a hook installing
``class CUE(YamlLexer)`` passes it with every block tokenised as YAML,
where ``//`` is not a comment and ``#Application`` is one.

Registering the lexer is the easy half.  The hard half is that the
registration must not be allowed to fail quietly, and by default it
does — ``pymdownx.highlight.Highlight.get_lexer`` reads, verbatim::

    try:
        lexer = get_lexer_by_name(language, **lexer_options)
    except Exception:
        lexer = None
    ...
    if lexer is None:
        lexer = get_lexer_by_name(self.default_lang or 'text', **lexer_options)

So a failed registration is a GREEN build: every CUE fence goes through
the ``text`` lexer, emits no spans, logs nothing, and the pages look
finished until someone notices that none of the CUE is coloured.  That
is precisely the failure class the documentation gate exists to remove,
which is why :func:`on_config` below does not merely register the lexer
— it resolves the alias and tokenises a sample to prove the
registration took, and raises when it did not.  Nothing else in the
toolchain will report this.
"""

from __future__ import annotations

from mkdocs.config.defaults import MkDocsConfig
from mkdocs.exceptions import PluginError
from pygments.lexer import RegexLexer, bygroups, include, words
from pygments.lexers import LEXERS, _lexer_cache, get_lexer_by_name
from pygments.token import (
    Comment,
    Keyword,
    Name,
    Number,
    Operator,
    Punctuation,
    String,
    Whitespace,
)


class CueLexer(RegexLexer):
    """Tokenise CUE (https://cuelang.org).

    Scoped to what this corpus actually writes — package and import
    clauses, definitions (``#Application``), optional fields
    (``replicas?:``), unification ``&``, disjunction ``|`` with its
    ``*`` default marker, bounds (``>=0``), regex constraints (``=~``),
    ellipsis (``[...string]``), strings including the ``\"\"\"`` form,
    ``//`` comments and numbers with CUE's IEC multipliers (``200Gi``).

    It is deliberately not a complete CUE grammar: the job is to make a
    struct read as a struct, not to decide validity — ``cue vet``
    already does that, and the documentation gate already runs it over
    the complete manifests in these pages.
    """

    name = "CUE"
    url = "https://cuelang.org"
    # `get_lexer_by_name` lowercases its argument before matching, so a
    # ```CUE fence resolves through this same alias — no second entry.
    aliases = ["cue"]
    filenames = ["*.cue"]
    mimetypes = ["text/x-cue"]

    tokens = {
        "root": [
            (r"\s+", Whitespace),
            (r"//[^\n]*", Comment.Single),
            # An attribute: @tag(foo), @embed(file=…).
            (r"@[A-Za-z_]\w*", Name.Decorator),
            # Strings first, so nothing below can claim an opening quote.
            (r'"""', String, "multiline-string"),
            (r"'''", String.Other, "multiline-bytes"),
            (r'"', String, "string"),
            (r"'", String.Other, "bytes"),
            # A definition (#Application) or a hidden one (_#Backend).
            # Leading `#` is a definition in CUE, never a comment.
            (r"_?#[A-Za-z_][\w$]*", Name.Class),
            # A field name: an identifier before `:`, optionally marked
            # optional (`image?:`).  This rule is what makes a CUE block
            # read as a struct rather than a wall of identifiers, and it
            # tokenises to Name.Tag so a CUE block and the YAML block
            # next to it colour their keys alike.  It precedes the
            # keyword rules on purpose: a lookahead colon settles the
            # question of what the token is.
            (
                r"([A-Za-z_$][\w$]*)([ \t]*)(\??)(?=[ \t]*:)",
                bygroups(Name.Tag, Whitespace, Punctuation),
            ),
            (words(("package", "import", "let", "if", "for", "in"), suffix=r"\b"), Keyword),
            (
                words(
                    ("bool", "bytes", "float", "int", "number", "rune", "string", "uint"),
                    suffix=r"\b",
                ),
                Keyword.Type,
            ),
            (words(("false", "null", "true"), suffix=r"\b"), Keyword.Constant),
            (
                words(("and", "close", "div", "len", "mod", "or", "quo", "rem"), suffix=r"\b"),
                Name.Builtin,
            ),
            # Numbers.  Radix-prefixed forms first, then the float form
            # (which needs its dot to win over the selector dot below),
            # then integers.  The trailing group is CUE's multiplier
            # suffix, as in `memory: 200Gi`.
            (r"0[xX][0-9a-fA-F_]+", Number.Hex),
            (r"0[bB][01_]+", Number.Bin),
            (r"0[oO][0-7_]+", Number.Oct),
            (r"[0-9][\d_]*\.[\d_]*([eE][+-]?[\d_]+)?([KMGTPE]i?)?", Number.Float),
            (r"\.[0-9][\d_]*([eE][+-]?[\d_]+)?", Number.Float),
            (r"[0-9][\d_]*([eE][+-]?[\d_]+)?([KMGTPE]i?)?", Number.Integer),
            # `...` before the selector `.`, or an ellipsis lexes as
            # three selectors.
            (r"\.\.\.", Operator),
            (r"=~|!~|==|!=|<=|>=|&&|\|\||[&|*+\-/<>=!]", Operator),
            (r"[{}\[\](),;:.?]", Punctuation),
            # A hidden field (_needs); reached only when no colon
            # follows, since the field rule above claims that case.
            (r"_[\w$]*", Name.Variable),
            (r"[A-Za-z_$][\w$]*", Name),
        ],
        # A quoted CUE string cannot span a line — only the `"""` form
        # can — so an unterminated one pops at the newline instead of
        # swallowing the rest of the block.  A truncated snippet then
        # costs one mis-coloured line, not a whole page.
        "string": [
            (r'"', String, "#pop"),
            (r"\n", String, "#pop"),
            (r"\\\(", String.Interpol, "interpolation"),
            include("escapes"),
            (r'[^"\\\n]+', String),
            (r"\\", String),
        ],
        "bytes": [
            (r"'", String.Other, "#pop"),
            (r"\n", String.Other, "#pop"),
            (r"\\\(", String.Interpol, "interpolation"),
            include("escapes"),
            (r"[^'\\\n]+", String.Other),
            (r"\\", String.Other),
        ],
        "multiline-string": [
            (r'"""', String, "#pop"),
            (r"\\\(", String.Interpol, "interpolation"),
            include("escapes"),
            (r'[^"\\]+', String),
            (r'["\\]', String),
        ],
        "multiline-bytes": [
            (r"'''", String.Other, "#pop"),
            (r"\\\(", String.Interpol, "interpolation"),
            include("escapes"),
            (r"[^'\\]+", String.Other),
            (r"['\\]", String.Other),
        ],
        "escapes": [
            (r"\\u[0-9a-fA-F]{4}", String.Escape),
            (r"\\U[0-9a-fA-F]{8}", String.Escape),
            (r"\\x[0-9a-fA-F]{2}", String.Escape),
            (r"\\[0-7]{3}", String.Escape),
            (r"""\\[abfnrtv\\/'"]""", String.Escape),
        ],
        # An interpolation holds an expression, so it lexes as one; the
        # closing paren pops before `root` can call it punctuation.
        "interpolation": [
            (r"\)", String.Interpol, "#pop"),
            include("root"),
        ],
    }


# Exercises, in order: a comment, the `package` keyword, an import with
# a string, a definition reference, the unification operator, field
# names (plain and optional), a nested string, an integer and a bound.
# Every token kind the self-check demands appears here, and the check is
# only as good as this sample — extend it before relaxing the check.
SAMPLE = """\
// A sample, not an example: this is what proves the lexer resolved.
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

app: v1alpha1.#Application & {
    metadata: name: "sample"
    spec: base: {
        image:     "ghcr.io/my-org/my-service:1.0.0"
        replicas?: int & >=1
    }
}
"""

# (label, token family) pairs the sample must produce.  Membership uses
# pygments' token hierarchy (`Token.Comment.Single in Token.Comment`),
# so the test is at FAMILY granularity — and that cuts both ways, which
# is worth stating precisely rather than in the flattering direction.
#
# It does NOT fire when a construct is retokenised to a sibling subtype.
# That is the point: the failure being guarded is "the alias resolved to
# nothing and everything came back as Text", and a refinement of this
# lexer is not a defect.
#
# It equally does NOT fire when one construct inside a family is lost
# while some other rule still contributes that family. Delete the
# field-name rule and `Name.Tag` disappears from the output, but the
# bare-identifier rule at the bottom of `root` still produces `Name`, so
# this check stays green while every CUE block on the site loses the
# colouring that makes a struct read as a struct. Tightening to exact
# subtypes would close that at the cost of the property above — every
# future refinement would fail the build — so the granularity is kept
# and the limit is recorded: this check answers "did a lexer take?",
# not "is this lexer good?". The second question is review's.
_REQUIRED_TOKENS = (
    ("comment", Comment),
    ("keyword", Keyword),
    ("string", String),
    ("name", Name),
    ("number", Number),
    ("operator", Operator),
)


def _register() -> None:
    """Put :class:`CueLexer` where ``get_lexer_by_name`` will find it.

    pygments has no public registration API — third-party lexers are
    discovered through setuptools entry points, which a build hook
    loaded from a path in the docs tree does not have.  What it does
    have is the lookup itself, which in 2.20.0 reads::

        for module_name, name, aliases, _, _ in LEXERS.values():
            if _alias.lower() in aliases:
                if name not in _lexer_cache:
                    _load_lexers(module_name)
                return _lexer_cache[name](**options)

    ``_lexer_cache`` is keyed by ``cls.name``.  Seeding it first and
    adding the ``LEXERS`` row second means the row's module path is
    never imported — which matters, because mkdocs loads this file by
    path and ``__name__`` need not be importable.  If a future pygments
    drops that skip, the import fails loudly inside :func:`on_config`
    rather than silently, because the lookup there is not swallowed.
    """
    _lexer_cache[CueLexer.name] = CueLexer
    LEXERS[CueLexer.__name__] = (
        __name__,
        CueLexer.name,
        tuple(CueLexer.aliases),
        tuple(CueLexer.filenames),
        tuple(CueLexer.mimetypes),
    )


def on_config(config: MkDocsConfig) -> MkDocsConfig:
    """Register the lexer, then prove it took.

    ``pymdownx.highlight`` swallows a ``ClassNotFound`` and renders the
    block as plain text, so a failed registration is a green build with
    every CUE fence unhighlighted and nothing in the log.  The checks
    below are the only thing standing between that and a reader, so
    they raise ``PluginError`` — which fails the build with a message
    instead of a traceback.
    """
    _register()

    try:
        lexer = get_lexer_by_name("cue")
    except Exception as exc:  # ClassNotFound, or an import raised by _load_lexers
        raise PluginError(
            f"the CUE lexer did not register: resolving the `cue` alias still "
            f"raises {type(exc).__name__}: {exc}. Every ```cue fence would render "
            "through the `text` lexer and the build would otherwise stay green "
            "— see the module docstring in docs/hooks/cue_lexer.py."
        ) from exc

    if not isinstance(lexer, CueLexer):
        raise PluginError(
            "the `cue` alias resolves to "
            f"{type(lexer).__module__}.{type(lexer).__qualname__}, not this "
            "hook's CueLexer — something else claimed the alias, and what it "
            "makes of CUE is unknown. Check pygments' LEXERS table."
        )

    kinds = {tok for tok, _ in lexer.get_tokens(SAMPLE)}
    missing = [
        label for label, family in _REQUIRED_TOKENS if not any(k in family for k in kinds)
    ]
    problems = []
    if len(kinds) < 3:
        plural = "" if len(kinds) == 1 else "s"
        problems.append(f"it produced {len(kinds)} token kind{plural} in all")
    if missing:
        problems.append("it produced no " + ", ".join(missing))
    if problems:
        raise PluginError(
            "the CUE lexer registered but tokenised the sample into nothing "
            f"usable: {'; and '.join(problems)}. It would render as plain text, "
            "which is what a missing lexer looks like — and `pymdownx.highlight` "
            "catches the failure itself, so nothing else would report this. "
            "See SAMPLE and _REQUIRED_TOKENS in docs/hooks/cue_lexer.py."
        )

    return config
