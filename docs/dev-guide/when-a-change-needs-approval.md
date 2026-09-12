---
description: "The manifest edits that pause instead of rolling out, what you see when one does, and how to get it moving again."
---

# When a change needs approval

Most edits to `apprafter/Application.cue` roll out the moment you push. Some do
not: they are held, the previous version keeps serving, and nothing happens
until a human approves. This page is the list, and what to do when you hit one.

You will not lose anything. A held change is held, not rejected — your commit
is in Git, the running app is the one from before it, and approving releases
exactly what you pushed.

## The edits that hold

Sixteen, and the ones people are surprised by are in the second group —
adding something feels safe, and some additions change who can reach your
application or what gets pulled into it.

### Something is taken away

| You edit | What is at stake |
| --- | --- |
| Remove a `needs.*` entry | the database, volume or credential behind it. Approving starts a seven-day retention window; nothing is deleted while the change is held |
| Change `expose.network` from `public` to `internal` or `vpn` | the app stops being reachable from outside |
| Remove a hostname from a public app, or swap it for another | the old hostname stops resolving to you |
| Set `replicas` to `0` | a deliberate outage |
| Remove an `env` key whose value is a `claim.…` or `secret:"…"` reference | whatever read it loses its value |

### Something is added, and it widens what is exposed

| You edit | What is at stake |
| --- | --- |
| Change `expose.network` to `public` | your app goes onto the public Gateway |
| Add a hostname to a public app — including its first | a new address answers as you |
| Change `expose.port` on a public app | the public route now targets a different process |
| Point `image` at a different **repository** (the path, not the tag) | a different pull source |
| Relax `imagePolicy.resolve` from `off` on a tag-referenced image | a pinned reference goes back to floating |
| Add a `secret:"name/key"` reference to an `env` key | a secret reaches a container that did not have one |
| Turn an `env` reference back into a literal | a value that was indirect is now in Git |
| Re-point an existing `secret:` reference at a different secret | a different secret reaches the same variable |
| Add a `needs.jetstream.consume` entry naming another application | you and that application now share a stream, and each can disturb the other's consumer on it |
| Turn `needs.jetstream.dynamicStreams` on | your application can then read every stream in its namespace, and drain every work queue in it |
| Declare a stream subject outside your own prefix, or set `allowPurge` on a stream that already has one | you collect — or, with `allowPurge`, destroy — messages belonging to another application |

**Changing only the image tag does not hold.** That is the ordinary deploy —
see [Image iteration](image-iteration.md). Neither does adding a `needs`, adding
a literal or `claim.…` env var, scaling up from zero, removing an env literal,
or any hostname or port edit on a non-public app.

## What you see

Your push syncs, and then stops. `apprafter app status` is where it says so:

```sh
apprafter app status my-service
```

```text
Phase:  AwaitingMigrationApproval
Ready:  False (MigrationPending) — waiting on migration plan my-service-a1b2c3
```

In Argo CD the application tile reads **Degraded** — that is the gate holding,
not a broken deploy. The pods from before your edit are still serving.

## Getting it moving

Someone with cluster access approves it:

```sh
apprafter migration list
apprafter migration approve my-service-a1b2c3
```

Then your change applies and the rollout completes. The operator-side view —
who can approve, what the plan record contains, and how platform upgrades use
the same mechanism — is [Migration plans](../operator-guide/migration-plans.md).

**There is no reject for your own application.** If you did not mean the edit,
revert the commit and push; the plan is cleaned up on its own. That is
deliberate: the manifest in Git is what the cluster converges to, so undoing a
change means changing the manifest, not overriding it from the side.

## If two of your changes are in one push

The plan lists every trigger the diff produced, and its headline is the most
severe of them. A push that removes a `needs.pg` *and* makes the app public is
one plan naming both — approving it approves both. Push them separately if you
want to decide them separately.

## See also

- [Writing Application.cue](application-cue.md) — every field named above.
- [Image iteration](image-iteration.md) — the edits that do not hold, and why a
  moved tag is one of them.
- [The approval gate](../how-it-works/the-approval-gate.md) — why some changes
  gate and others do not, where a plan lives, and the lifecycle it moves
  through.
- [Migration plans](../operator-guide/migration-plans.md) — the operator's side:
  approving, the plan record, and platform-scope plans.
