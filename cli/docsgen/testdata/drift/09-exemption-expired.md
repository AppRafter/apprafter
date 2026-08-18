---
cli-check-ignore:
  - span: "apprafter node reserve-headroom"
    reason: historical
    since: v0.1.0
    note: names the removed command so scripts calling it can be migrated
---

# An exemption past the window

The claim this exemption makes was true when it was taken and may not be
now, so it is reported **and** it stops silencing: an exemption that never
expires is how a two-year-old `known-broken` comes to hide a live finding.
The invocation it covered is therefore reported as well.

`apprafter node reserve-headroom` was removed. Use `apprafter node prep`.
