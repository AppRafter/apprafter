---
description: "Running one manifest as a staging and a production deployment: what an environment selects, how an override merges onto the base, and which commands need to be told which one you mean."
---

# Deploying more than one environment

One repository, one manifest, two running copies with different replica
counts, different log levels and different databases — that is what an
environment is for here. You do not fork the manifest, keep a
`staging` branch of it, or run a second cluster.

This page covers what an environment selects and what it does not, how
to register the same manifest twice, how an override merges onto the
base, how the cluster's default environment behaves, and how to tell
the two deployments apart afterwards.

The decision behind the model is
[Per-environment deploy](../how-it-works/per-environment-deploy.md).

## What an environment is here

An environment is a **property of a deployment**, chosen when you
register it. Your manifest declares the overrides it supports under
`Application.spec.environments`; `apprafter app add --env <name>` picks
one, and each pick produces a separate, self-contained deployment:

- its own Argo CD application, named `<app>-<env>`;
- its own `Application` resource, in a namespace you choose;
- its own Git revision, so staging can track your default branch while
  production tracks a tag;
- its own provisioned dependencies — two namespaces means two claims,
  so a `needs.pg` declaration gives each environment its **own
  database**, not a shared one.

The manifest itself stays environment-agnostic. Nothing in it says
"this is production": the same commit is what both deployments read.

## What an environment is not

**It is not a namespace-per-team model.** The namespace is a separate
choice you make at `apprafter app add` time, and the environment never
derives it or constrains it. You can name namespaces anything you like.
The one rule that follows from the model is in the other direction: two
environments of the same app cannot share a namespace, because both
render an `Application` under the same `metadata.name` and would be the
same object.

**It is not a mode the cluster is in.** There is no switch that puts a
cluster into "staging". A cluster runs whatever mix of deployments you
register on it, and the cluster-wide default environment (below) only
preselects a prompt.

**It is not a Git branch.** Which branch or tag a deployment follows is
`--branch`, an independent per-deployment setting. An environment
selects an *override block*; a branch selects a *revision*. They are
routinely combined, but neither implies the other.

**It is not a security boundary between tenants.** It separates one
app's deployments from each other. Isolation between different teams'
workloads is a namespace and network-policy question — see the
[egress guide](../operator-guide/egress-policy.md).

## Declare the overrides

`Application.spec.environments` is a map from environment name to a
**partial** override. Every field in it is optional: an environment
carries only its diff from `spec.base`.

```cue
package apprafter

import v1alpha1 "apprafter.io/schemas/v1alpha1"

parser: v1alpha1.#Application & {
	metadata: name: "parser"

	spec: {
		base: {
			image:    "ghcr.io/acme/parser:1.4.0"
			replicas: 2
			expose: {
				port:    8080
				network: "internal"
			}
			env: LOG_LEVEL: "info"
		}

		environments: {
			staging: {
				replicas: 1
				env: LOG_LEVEL: "debug"
			}
			prod: {
				replicas: 4
				expose: {
					network:  "public"
					hostname: "parser.example.com"
				}
			}
		}
	}
}
```

An override may carry anything `spec.base` carries, which is what makes
"production is the public one" expressible: only `prod` above asks for a
public route, and it needs a hostname under a zone the cluster has
already been given — see [connect a
domain](../operator-guide/connect-a-domain.md).

A real one, for comparison: the manifest behind this documentation site
declares a single `dev` override — one replica, and `network: "internal"`,
because two deployments must not both claim `docs.apprafter.dev` — and records
why there is deliberately no `prod` entry, since `base` is already the
production shape:
[Worked example: deploying this documentation site](../how-it-works/deploying-the-docs-site.md#the-manifest-in-full).
It is registered as a single base-only deployment, so it still pins
`metadata.namespace` — the one field the section below tells you to drop before
you deploy a second environment.

Environment names are DNS-1123 labels — lowercase letters, digits and
`-`, starting and ending alphanumeric. The admission webhook rejects
anything else, and so does the CLI, because the name becomes part of the
Argo CD application's own name.

Check the file before you push it:

```sh
apprafter app validate
```

That runs the same CUE evaluation the cluster runs at sync time, so a
misspelled field in an override is a local error rather than a failed
sync. It needs `cue` on your `PATH`.

### Leave `metadata.namespace` out

This is the one authoring detail that decides whether a second
environment can be deployed at all.

A manifest that pins `metadata.namespace` renders that same namespace
for every deployment of it. Two environments would then produce the same
object — same kind, same name, same namespace — and the two Argo CD
applications would each try to own it.

Omit the field, and the namespace comes from whatever you pass to
`apprafter app add --namespace`, per deployment. One manifest then lands
as two independent `Application` resources in two namespaces.

!!! warning "The scaffold writes one"

    `apprafter app scaffold` emits a `metadata.namespace: "apprafter"`
    line. Delete it before you deploy a second environment. The
    commented-out `environments` example the scaffold also emits
    predates the current field set — write the block from the shape
    above instead of uncommenting that one.

## Deploy the same manifest twice

Run `apprafter app add` once per environment, from the directory holding
your `apprafter/` manifest, giving each its own namespace:

```sh
apprafter app add --env staging --namespace parser-staging
apprafter app add --env prod    --namespace parser-prod
```

Each call registers one Argo CD application, named for the app plus the
environment. The app half comes from `--name`, or from the repository's
own name when you omit it — so a `parser` repository gives
`parser-staging` and `parser-prod`. Argo CD creates the destination
namespace on first sync.

Add `--branch` to either call to point that deployment at a different
revision:

```sh
apprafter app add --env staging --namespace parser-staging --branch main
apprafter app add --env prod    --namespace parser-prod    --branch v1.4.0
```

`--env` is checked against the environments your manifest declares
before anything is written to the cluster:

```text
environment 'qa' is not declared in this app's manifest. Declared
environments: prod, staging. Add a `spec.environments.qa` block to
apprafter/Application.cue, or pass one of the declared environments to
`--env`.
```

If the manifest cannot be evaluated at all — most often because `cue` is
not on your `PATH` — the check is skipped with a warning and the value
is passed through. Nothing downstream re-checks it: a deployment naming
an environment the manifest does not declare is accepted and renders
`spec.base` alone.

### Deploying without an environment

Omitting `--env` registers a **base-only** deployment: the Argo CD
application is named `<app>` with no suffix, and no override is applied.
That is the right shape for an app that has no `environments` block at
all.

On a terminal, `apprafter app add` runs its wizard, and when the
manifest declares environments the wizard asks you to pick one — there
is no "none" entry in that list. To register a base-only deployment of a
manifest that *does* declare environments, pass `--no-interactive` and
no `--env`.

## How an override merges onto the base

`image`, `replicas` and `expose` replace; `env` merges key by key with the
override winning. `expose` merges by subfield, so an override that sets only
`network` keeps the base's `port`.

The full rules, and what the operator does with them before rendering, are
[Per-environment deploy](../how-it-works/per-environment-deploy.md).

## The cluster's default environment

A cluster can record a default:

```sh
apprafter platform env show
apprafter platform env set prod
```

It is stored as `PlatformStack.spec.defaultEnvironment`, and it is
**soft** — a convenience, not a constraint. Concretely, it does two
things and nothing else:

- it preselects the cursor in the `apprafter app add` environment
  picker, when the manifest declares that environment;
- it is echoed by `apprafter app add` when you register without `--env`,
  so you can see what you did not pin.

It does **not** change rendering. A deployment registered without
`--env` is base-only whatever the default says, and `--env` always wins
over it. Both commands print the caveat with their output:

```text
Default environment: prod
(soft default — preselects the `apprafter app add` env picker; it does
NOT change rendering. An app added without `--env` is still base-only.)
```

## Seeing which environment a deployment is in

`apprafter app list` carries an `ENV` column. Each environment is its own
row, and a base-only deployment reads `(base)`:

```sh
apprafter app list
```

```text
+----------------+---------+----------------+-----------+----------------------------+--------+---------+
| NAME           | ENV     | NAMESPACE      | WORKLOADS | REPO                       | SYNC   | HEALTH  |
+----------------+---------+----------------+-----------+----------------------------+--------+---------+
| parser-prod    | prod    | parser-prod    | 1         | github.com/acme/parser.git | Synced | Healthy |
+----------------+---------+----------------+-----------+----------------------------+--------+---------+
| parser-staging | staging | parser-staging | 1         | github.com/acme/parser.git | Synced | Healthy |
+----------------+---------+----------------+-----------+----------------------------+--------+---------+
HEALTH is Argo CD's verdict on the Application CRs; it does not see pod state — run `apprafter app status <name>`.
```

`WORKLOADS` is how many workloads that one registration deploys — a
manifest package is a bundle, and `1` here says each of these two is a
single-workload one. The repo URL drops its `https://` for width;
`PROJECT` and `REV` are not in the list, but `apprafter app status`
still prints both.

`HEALTH` is folded over all of a registration's workloads. When they
agree it is the single word above; when they do not, the cell names the
worst one and how many are in it — `Degraded 1/3` is a different
situation from `Degraded 3/3`. A workload held at a digest by `apprafter
app rollback` adds `· 1 pinned`, because a pin is otherwise invisible
next to a sibling that is still rolling. As the footer says, this column
is a verdict on the `Application` CRs and does not look at pods — for
that, run `apprafter app status <name>`.

**Read the logical name off that table by stripping the `ENV` suffix**,
because no column carries it: `NAME` is the per-environment identity
`<app>-<env>`, which is the Argo CD application's own name. Here the
logical name is `parser`, and it is `parser` you type at every
`apprafter app` command below.

`apprafter app status` takes the **logical** name and aggregates: it
finds every environment of that app and prints a full detail block for
each, separated by a rule. When there is more than one, a header lists
them first.

```sh
apprafter app status parser
```

```text
Application 'parser' — 2 environment deployments:
  • parser-prod (prod)
  • parser-staging (staging)
```

Every detail block carries its own `environment:` line, so an app with
one environment shows which one it is too.

A detail block describes one **workload**, and one deployment may hold
more than one — that is what `WORKLOADS` counts in the table above. When
it holds two or more, the block is a summary instead: one row per
workload with its phase, its ready pod count and the image it is
running, and underneath it a line saying how many are not `Ready`. That
line is printed only when some are not, so its presence is the signal.
Pass `--workload <name>` for the full block of one:

```sh
apprafter app status parser --workload parser-worker
```

The positional argument is always the **application**, never a workload.
At one workload per package the two names are usually the same string,
which is why the rule is fixed rather than guessed: `--workload` is the
only way to name a workload, in the same sense `--env` names an
environment.

## Working with one environment

Every `apprafter app` command takes the **logical** name — `parser`. It
is what you passed to `apprafter app add --name`, or, when you passed no
`--name`, the repository's own last path segment: `.git` dropped,
lowercased, and every character that is not a letter, digit or `-` folded
to `-` (so a `https://github.com/acme/parser.git` remote gives `parser`,
and `.../My_App` gives `my-app`). It is **not** a column of
`apprafter app list`,
which shows the per-environment `<app>-<env>` identity instead — see
[above](#seeing-which-environment-a-deployment-is-in).

You never type `parser-prod`. `--env` exists only to say *which*
deployment you mean, and only when there is more than one:

| Command | `--env` | Without it |
| --- | --- | --- |
| `apprafter app add` | selects the override | registers a base-only deployment |
| `apprafter app list` | — | one row per deployment, with the `ENV` column |
| `apprafter app status` | — | aggregates every environment of the app |
| `apprafter app logs` | picks one | resolves a single deployment; with two or more it stops and lists them |
| `apprafter app open` | picks one | same |
| `apprafter app rollback` | picks one | same |
| `apprafter app unpin` | picks one | same |
| `apprafter app remove` | removes one | removes **every** environment of the app |

So a one-environment app never needs `--env`:

```sh
apprafter app logs parser
```

and a two-environment app is told which one:

```sh
apprafter app logs parser --env prod
apprafter app open parser --env staging
```

Leaving it out where it is needed is an error that names the choices
rather than guessing:

```text
'parser' is deployed per environment (prod, staging). Pass `--env <env>`
to target one.
```

Rollbacks are per environment for the same reason a revision is:
`apprafter app rollback parser --env prod` acts on that deployment and
leaves staging where it is. An image pin is per environment too — one
environment can be held at an older build while the other keeps
following the tag — so `apprafter app unpin` takes `--env` on the same
terms.

## Removing an environment

`--env` removes one deployment and leaves the others running:

```sh
apprafter app remove parser --env staging --yes
```

Without `--env`, removal is logical — it tears down **every**
environment of the app. On a terminal it lists them and asks once
before doing so; `--yes` skips that prompt, so in a script the
un-suffixed form is the destructive one:

```sh
apprafter app remove parser --yes   # removes staging AND prod
```

Pass `--keep-data` to preserve provisioned volumes and claims through
the teardown.

## Changing a deployment's environment

There is no command that moves a running deployment from one
environment to another, and none that copies an image from one to
another. The environment is part of the deployment's identity — it
names the Argo CD application and selects the override — so changing it
means replacing the deployment:

```sh
apprafter app remove <app> --env <old-env> --yes
apprafter app add --env <new-env> --namespace <namespace>
```

!!! danger "The data does not move with the deployment"

    Replacing a deployment replaces its dependencies too. Removing the
    old one deletes its `ResourceClaim`s, and the new one provisions
    fresh, empty ones — a `needs.pg` database in the new environment
    starts with no rows in it.

    What happens to the old data: a finalizer snapshots each claim into
    an immutable `RetainedClaim` and holds the database and its role for
    **seven days**. After that the collector drops the role, the
    database, the password Secret and the snapshot. So the window to
    recover anything is a week, and nothing warns you when it closes.

    `--keep-data` is not the answer here. It strips the cascade
    finalizer so the removal does not prune the synced AppRafter
    resource — which keeps the old environment's workload running, no
    longer managed by Argo CD. That is useful when you intend to
    re-attach data after a teardown; it is not a way to carry data from
    one environment to another.

    **If the new environment needs the old data, move it yourself
    before removing anything** — take a dump from the old database and
    load it into the new one once its claim is `ready`.

Editing the `spec.environments.staging` block and pushing, on the other
hand, needs no CLI step at all: Argo CD syncs the change and the
operator re-renders that deployment. Only the *choice* of environment is
fixed at registration time.

## Where to look next

- [Writing Application.cue](application-cue.md) — every field an
  override may carry, and the `needs` and `env` reference syntax.
- [Image iteration](image-iteration.md) — how each deployment picks up
  a re-pushed tag independently.
- [`apprafter app`](../reference/cli/app.md) and
  [`apprafter platform`](../reference/cli/platform.md) — the generated
  flag-by-flag reference.
