---
description: "How an environment override merges onto the base, what an environment is and is not in this platform, and why the same manifest deployed twice gives two independent applications."
---

# Per-environment deploy

How one manifest becomes two deployments, and what the merge actually does. The
recipe is
[Deploying more than one environment](../dev-guide/environments.md); none of
this is needed to write one.

Read it when an override produced a spec you did not expect — most often
because a field you assumed replaces in fact merges, or the other way round.

The decision is
[ADR 0044](../adr/0044-per-environment-deploy.md): the environment is a
property of a **deployment**, not of the manifest, which is what makes the same
file usable twice without a branch.

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

## How an override merges onto the base

The operator computes the running spec as `spec.base` with the selected
environment folded onto it, field by field — so an override that sets
one subfield does not blank its neighbours. In short: `image` and
`replicas` replace; `expose` and `imagePolicy` merge per subfield;
`resources` merges per key inside `requests` and `limits`; `env` merges
with the environment winning a shared key; `needs` replaces per service
key.

The per-field rule for everything an override may carry is stated once,
in [Writing
Application.cue](../dev-guide/application-cue.md#multi-environment-patterns) — read
it there rather than inferring it from the two tables below.

For the manifest example above, the `staging` deployment runs:

| Field | Value | From |
| --- | --- | --- |
| `image` | `ghcr.io/acme/parser:1.4.0` | base — staging sets none |
| `replicas` | `1` | staging replaces base's `2` |
| `expose.port` | `8080` | base |
| `expose.network` | `internal` | base |
| `env.LOG_LEVEL` | `debug` | staging wins the key |

and the `prod` deployment runs:

| Field | Value | From |
| --- | --- | --- |
| `image` | `ghcr.io/acme/parser:1.4.0` | base |
| `replicas` | `4` | prod |
| `expose.port` | `8080` | base — inherited through the subfield merge |
| `expose.network` | `public` | prod |
| `expose.hostname` | `parser.example.com` | prod |
| `env.LOG_LEVEL` | `info` | base — prod overrides nothing here |

## See also

- [Deploying more than one environment](../dev-guide/environments.md) — the
  recipe, and the commands that need telling which deployment you mean.
- [Writing Application.cue](../dev-guide/application-cue.md) — the fields an
  override may set.
