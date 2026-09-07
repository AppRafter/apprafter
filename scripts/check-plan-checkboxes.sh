#!/usr/bin/env bash
# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# An open plan item under a closed one must say what happened to it.
#
# `plan.md` is the phase ledger, and its checkboxes are how the loop
# decides what is left. Two shapes make it lie, and a reader found both
# rather than a machine — twice in one session, which is what earned this
# script:
#
#   * `2.23n` was `- [x]` with three `- [ ]` children that described work
#     already done and were simply never flipped;
#   * `2.18`'s heading shouted the file's closure word over four open
#     boxes, two of which had been delivered in a different shape than
#     planned.
#
# `plan.md` is written in Russian, so the two vocabularies matched below
# are built from escape sequences rather than written out. That keeps
# this file English-only for `check-no-cyrillic.sh`, which solves the
# same problem the same way.
#
# Neither is visible to the docs gate: `plan.md` is a root working file
# and deliberately out of the published corpus.
#
# WHAT IS NOT REPORTED, and why the check is usable. An open child under
# a closed parent is the corpus's normal way of recording a batch that
# landed with named pieces deferred — nine of the ten in the file at the
# time of writing. Those all say where the piece went ("Deferred to
# 1.79a", "post-launch", an arrow to another subphase, a version). So
# the rule is about DISPOSITION, not about the box: an open
# item is fine when it says what became of it, and reported when it just
# sits there.
#
# The remedy is never to flip a box to silence this. Verify the item
# against the TREE — a checkbox is exactly the thing that lags — and
# either check it with the evidence, or record where the work went.
set -euo pipefail

PLAN="${1:-plan.md}"
[ -f "$PLAN" ] || { echo "OK: no $PLAN to check."; exit 0; }

python3 - "$PLAN" <<'PY'
import re, sys

path = sys.argv[1]
lines = open(path, encoding="utf-8").read().split("\n")

BOX = re.compile(r'^(\s*)- \[( |x)\] ')
HEADING = re.compile(r'^(#{2,4}) ')
# The three inflections of the file's closure word, UPPER CASE, plus the
# English one. Case matters: the announcement is shouted and the ordinary
# noun is not, so a case-insensitive match reads a heading that NAMES the
# task of closing something as a claim that it IS closed. One such
# heading exists (`2.17`), and it was this check's first false positive.
_CLOSED = "|".join((
    "\u0417\u0410\u041a\u0420\u042b\u0422\u041e",
    "\u0417\u0410\u041a\u0420\u042b\u0422\u0410",
    "\u0417\u0410\u041a\u0420\u042b\u0422\u042b",
    "CLOSED",
))
CLOSED_HEADING = re.compile(r'^#{2,4} .*\b(' + _CLOSED + r')\b')

# Where the work went. Deliberately broad: a false accept costs one
# unflipped box that a reader still catches, while a false reject trains
# everyone to ignore the check.
_DISPOSITION = (
    # English, as the file writes it.
    r"Deferred", r"Post-launch", r"blocked",
    # An arrow or a "see" pointing at another subphase, and a version.
    r"→\s*\d", r"\u0441\u043c\.\s*\d", r"\u0432\s*`?v\d",
    # Russian stems: deferred, moved, out of scope, horizon, open,
    # waiting, not in this one, and the noun for a pending decision.
    r"\u043e\u0442\u043b\u043e\u0436\u0435\u043d",
    r"\u043f\u0435\u0440\u0435\u043d\u0435\u0441",
    r"\u0432\u043d\u0435 \u043e\u0431\u044a\u0451\u043c\u0430",
    r"\u0433\u043e\u0440\u0438\u0437\u043e\u043d\u0442",
    r"\u043e\u0442\u043a\u0440\u044b\u0442",
    r"\u0436\u0434\u0451\u0442",
    r"\u043d\u0435 \u0432 \u044d\u0442\u043e\u043c",
    r"\u0440\u0435\u0448\u0435\u043d\u0438\u0435",
)
DISPOSITION = re.compile("|".join(_DISPOSITION), re.IGNORECASE)


def block_at(index):
    """A box line plus its wrapped continuation lines."""
    text = lines[index]
    for nxt in lines[index + 1:]:
        if BOX.match(nxt) or not nxt.strip() or HEADING.match(nxt):
            break
        text += " " + nxt
    return text


problems = []

# --- an undisposed open item under a closed parent ----------------------
# Ancestry by indent; a fence is skipped so an example list inside one is
# not read as the plan's own.
stack, fenced = [], False
for n, line in enumerate(lines, 1):
    if line.lstrip().startswith("```"):
        fenced = not fenced
        continue
    if fenced:
        continue
    m = BOX.match(line)
    if not m:
        continue
    indent, mark = len(m.group(1)), m.group(2)
    while stack and stack[-1][0] >= indent:
        stack.pop()
    if mark == " " and stack and stack[-1][2] == "x":
        if not DISPOSITION.search(block_at(n - 1)):
            problems.append(
                f"{path}:{n}: open item under a closed parent, with no note of "
                f"what became of it\n"
                f"    parent ({path}:{stack[-1][1]}): {lines[stack[-1][1] - 1].strip()[:96]}\n"
                f"    child:  {line.strip()[:96]}"
            )
    stack.append((indent, n, mark))

# --- a closure heading over undisposed open boxes -----------------------
# Bounded by the next heading of the same or shallower level, so a closed
# subphase is not blamed for the next one's open work.
for n, line in enumerate(lines, 1):
    if not CLOSED_HEADING.match(line):
        continue
    level = len(HEADING.match(line).group(1))
    stray = []
    for m2, l2 in enumerate(lines[n:], n + 1):
        h = HEADING.match(l2)
        if h and len(h.group(1)) <= level:
            break
        b = BOX.match(l2)
        if b and b.group(2) == " " and not DISPOSITION.search(block_at(m2 - 1)):
            stray.append((m2, l2.strip()[:96]))
    if stray:
        listed = "\n".join(f"    {path}:{m2}: {t}" for m2, t in stray[:5])
        more = "" if len(stray) <= 5 else f"\n    … and {len(stray) - 5} more"
        problems.append(
            f"{path}:{n}: heading announces closure over {len(stray)} open "
            f"item(s) that say nothing about where they went\n"
            f"    heading: {line.strip()[:96]}\n{listed}{more}"
        )

if problems:
    print("::error::plan.md claims work is closed that its own boxes say is not.\n")
    for p in problems:
        print(p + "\n")
    print(
        "Do NOT flip a box to silence this. Verify the item against the tree —\n"
        "a checkbox lags what shipped — then either check it with the evidence,\n"
        "or record where the work went.",
        file=sys.stderr,
    )
    sys.exit(1)

print("OK: every open plan item under a closed one says where it went.")
PY
