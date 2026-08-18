---
cli-check-ignore:
  - span: "apprafter node reserve-headroom"
    reason: historical
    since: v0.9.0
    note: names the removed command so scripts calling it can be migrated
---

# A fresh exemption, on a page that is correct as written

The front matter is identical to the drift twin's but for the `since:` —
same span, same typed reason — and the body names the same removed command
**on purpose**, which is what documenting a removal looks like. The
window, not the shape, is the difference.

`apprafter node reserve-headroom` was removed. Use `apprafter node prep`.
