---
description: "How the platform does what the guides ask it to do — the mechanism behind each operator recipe, for when you need to know."
---

# How it works

The operator and developer guides tell you what to run. These pages tell you
what happens when you do.

Nothing here is needed to operate AppRafter. Reach for a page when a guide's
outcome surprised you, when you are diagnosing something the troubleshooting
tables do not cover, or when you are changing the platform itself and need to
know what the current design is before you move it.

Each page names the code it describes, so a claim here can be checked rather
than taken on faith.

- [Declared Postgres dependencies](needs-pg.md) — how `needs.pg` becomes a
  database: the claim, the scheduler, the lazily-created shared cluster, the
  connection Secret, and the phased drop after the grace window.
- [Declared Redis dependencies](needs-redis.md) — how `needs.redis` becomes an
  isolated logical database on a shared pool, what the per-claim credential
  enforces, why channels are the one exception, and the flush after the grace
  window.
- [Declared disk dependencies](needs-disk.md) — how `needs.disk` becomes a
  mounted volume, why that volume has no owner and what the retention model
  rests on, and what single-writer storage forces on a rollout.

## Why this section exists

[ADR 0058](../adr/0058-public-surfaces-are-written-for-their-reader.md) makes a
guide a recipe: its main flow carries `apprafter` commands and genuinely
external tools, and everything else routes by role. Mechanism is one of those
roles, and this is where it goes. Before that decision the explanations lived
inline, interleaved with the steps — which made short procedures read as long
ones and made the platform look like it demanded Kubernetes fluency to use.
