# AppRafter CLI — DX Improvements Spec

> **Scope:** Improve `apprafter` CLI ergonomics for Tier 1 / single-developer workflows.
> **Status:** Spec for implementation. Owner reviews, agent executes.
> **Target phase:** v0.1.x (current). Not a Phase 2 task — should land before resuming Phase 2.

---

## 0. Context and motivation

Current CLI state (as of v0.1.6x):

- Bootstrap requires manually exporting `HCLOUD_TOKEN` and `APPRAFTER_SSH_PUBLIC_KEY` before each shell session
- These values do not persist across sessions
- A full Tier 1 bootstrap requires ~5 sequential commands copy-pasted from docs
- CLI binary name in code is still `platform-cli` (legacy), should be `apprafter`
- No interactive prompts, no validation feedback, no clear "what's wrong" UX
- No way to switch between Hetzner accounts or between dev/prod credentials

This spec addresses all of the above as a single coordinated DX pass.

---

## 1. Goals

### Primary

1. **Persistent credential storage** — credentials survive across shell sessions, reboots, terminal switches
2. **Multi-target support** — multiple named deployment targets (Hetzner account #1, account #2, future AWS, future AppRafter Cloud), one active at a time, easy switching
3. **Interactive-first onboarding** — `apprafter target add` wizard with prompts and validation
4. **Generic credential store** — designed for future providers (AWS, OpenBao, GitOps repos, container registries), not Hetzner-only
5. **Better bootstrap UX** — keep separate commands (`init`, `apply`, `cluster-bootstrap`), but add orchestrating wrapper for full-flow scenarios
6. **CLI binary rename** — `platform-cli` → `apprafter` everywhere (Cargo.toml, docs, plan.md, spec.md)

### Secondary

7. Validation on credential input (regex + API ping)
8. `doctor` subcommand for self-diagnostics
9. TTY detection — non-interactive mode for CI/scripts
10. Color/style support respecting `NO_COLOR`, `--color` flag
11. `miette`-style diagnostic errors

---

## 2. Non-goals

- Do **not** implement OS keyring backend yet (plain YAML with 0600 permissions is enough for v1; keyring is a follow-up)
- Do **not** implement encrypted credentials storage via age (deferred — keyring is the right next step, not age)
- Do **not** implement cloud-synced targets (shared between machines / team members) — manual sync via dotfiles is fine for now
- Do **not** build a full TUI — this is enhanced CLI, not k9s-style application
- Do **not** support credential rotation flows automatically (manual `apprafter target add <name> --renew` is the API)
- Do **not** introduce `apprafter login` — its semantics are ambiguous in a future with AppRafter Cloud (cloud auth) alongside self-hosted (target setup). See §3.4 for rationale.

---

## 3. CLI command structure decisions

### 3.1 Two layers, two command groups

`apprafter` distinguishes two conceptual layers:

**Identity (who am I)** — handled by `apprafter auth`. Currently a placeholder; will become OAuth-against-AppRafter-Cloud once the managed offering exists. In self-hosted mode, identity is implicit (the operator of the shell).

**Target (where do I deploy)** — handled by `apprafter target`. A Target is a named bundle of `(provider, region, credentials, default settings)`. The active Target is the one that subsequent operational commands (`init`, `apply`, etc.) act upon.

In the future, AppRafter Cloud becomes **just another Target type**, where infrastructure is managed remotely instead of locally. The Target abstraction stays the same.

### 3.2 Verb-first vs resource-first

The CLI uses **resource-first grouping for CRUD operations** (`apprafter target add` instead of `apprafter add target` or `apprafter add hetzner`).

Rationale:
- AppRafter has multiple distinct resources (Targets, Auth identities, Applications, ServiceProviders, AccessGrants, Clusters). A verb like `add` would be ambiguous about what's being added.
- Resource-first grouping is the kubectl/gh/aws pattern, familiar to most platform engineers.
- It improves discoverability for solo founders unfamiliar with the tool — `apprafter target add hetzner` is self-explanatory; `apprafter add hetzner` is a guess.

Common operational commands that are **not** CRUD (`init`, `apply`, `doctor`, `whoami`, `version`, `bootstrap-all`) stay top-level. They have unambiguous single meaning regardless of future expansion.

### 3.3 Short aliases for frequent users

Use `clap` aliases to provide short forms for users who know the tool:

- `apprafter t ...` → `apprafter target ...`
- (No alias for `apprafter auth` — `a` would conflict with `apply`)

Discoverability in `--help` shows the canonical form. Aliases are documented but not advertised at the top of help.

### 3.4 Why no `apprafter login`

The word `login` would be semantically conflicting:

- **Today (self-hosted only):** `login` would mean "set up credentials for my infrastructure provider"
- **Future (with managed):** `login` would naturally mean "authenticate to AppRafter Cloud account"

Pre-committing the word `login` to one meaning now creates a breaking-change debt later. Using `target add` and `auth login` from the start keeps both meanings unambiguous.

### 3.5 Why `target` over `provider`

The platform already uses the word "provider" in two contexts:
- `ServiceProvider` — a backend for a platform service type (e.g., `pg-integrated`, `pg-aws`)
- `InfrastructureProviderPlugin` — a plugin extending `apprafter` with support for an additional cloud

Adding a third meaning ("CLI target where I deploy") would dilute the term. `target` is unused elsewhere and clearly conveys "deployment destination".

---

## 4. Conceptual model

### Target

Named bundle of `(provider, region, credentials, defaults)` for a single deployment destination.

```
Target {
    name: String,
    config: TargetConfig,
    credentials: TargetCredentials,
}
```

A user has **one or more targets**, **one active** at a time. Default target name on first run: `default`.

### TargetConfig (non-secret)

```yaml
provider: hetzner-cloud       # which provider plugin
region: nbg1
sshKey: ~/.ssh/apprafter_ed25519.pub
defaultTier: solo
clusterName: my-cluster
# Future fields per provider plugin
```

### TargetCredentials (secret)

```yaml
hetznerToken: hcloud_xxxxx
# Future fields per provider plugin (AWS access key, OpenBao token, AppRafter Cloud refresh token, etc.)
```

### File layout

```
$XDG_CONFIG_HOME/apprafter/      # ~/.config/apprafter/ on Linux/Mac
├── config.yaml                  # global state: active target, defaults, CLI version
├── targets/
│   ├── default/
│   │   ├── config.yaml          # TargetConfig
│   │   └── credentials.yaml     # TargetCredentials, mode 0600
│   ├── work/
│   │   ├── config.yaml
│   │   └── credentials.yaml
│   └── ...
├── auth/                        # Reserved for `apprafter auth` (Managed identities, future)
│   └── .keep                    # empty for now, structure to be defined when Managed lands
└── state/                       # Runtime caches (cluster state, kubeconfigs, etc.)
    └── <target>/
        └── ...
```

Use `dirs::config_dir()` crate to resolve paths cross-platform. **Never** hardcode `~/.apprafter` or `~/.config/apprafter`.

### Global config

```yaml
# $XDG_CONFIG_HOME/apprafter/config.yaml
activeTarget: default
version: 1
```

The `version` field enables future migrations.

---

## 5. Subcommand specification

### 5.1 `apprafter target add [<name>]`

Interactive wizard to create a new target, or update an existing one.

**Interactive flow (default when TTY detected):**

```
$ apprafter target add

Welcome to AppRafter. Let's set up a deployment target.

? Target name: › default
? Provider: › Hetzner Cloud
? Hetzner API token: › ****
  ✓ Token format valid
  ✓ Token verified (account: myname@example.com, project: default)
? SSH public key: › ~/.ssh/id_ed25519.pub
  ✓ Found existing key
? Default region: › nbg1
? Default tier: › solo (€5-20/mo, single VPS)

✓ Target 'default' created and set active
ℹ Run `apprafter init` to provision your first cluster.
```

**Non-interactive flow (flags or `--no-interactive`):**

```bash
apprafter target add default \
  --provider hetzner-cloud \
  --token "$HCLOUD_TOKEN" \
  --ssh-key ~/.ssh/id_ed25519.pub \
  --region nbg1 \
  --tier solo \
  --no-interactive
```

In non-interactive mode: missing required flags → error with miette diagnostic pointing to which flag is missing.

**Validation steps (both modes):**

1. **Token format check** — per provider:
    - Hetzner Cloud: exactly 64 ASCII alphanumeric characters, no prefix. The token is what the Hetzner Cloud Console → Security → API Tokens panel emits when you click "Create API token"; the surrounding env var name `HCLOUD_TOKEN` is convention but does not appear inside the value. (Spec amended in v0.1.74 after v0.1.73 wrongly required an `hcloud_` prefix that Hetzner never issued.)
    - Future providers: their own patterns
2. **SSH key existence and readability**
3. **API ping** — verify token works against provider API:
    - Hetzner: `GET /v1/locations` (lightweight, no resources touched)
4. **Region validity** — must be in provider's known regions list
5. **Target name** — alphanumeric + `-`, no spaces, max 64 chars

**Failure modes:**

- Target already exists with different config → prompt "Overwrite?" (interactive) or fail without `--force` (non-interactive)
- Token invalid → show miette diagnostic with link to provider's token page
- SSH key not found → offer to generate one (`ssh-keygen -t ed25519 -f ~/.ssh/apprafter_ed25519 -N ""`)

**Renewing credentials for existing target:**

```bash
apprafter target add default --renew    # interactive: prompt only for new token
apprafter target add default --renew --token "$NEW_TOKEN" --no-interactive
```

### 5.2 `apprafter target list`

Show all configured targets.

```
$ apprafter target list

┌─────────┬─────────────────┬─────────────────┬────────┬──────────────────────┐
│ Active  │ Name            │ Provider        │ Region │ Last used            │
├─────────┼─────────────────┼─────────────────┼────────┼──────────────────────┤
│ *       │ default         │ hetzner-cloud   │ nbg1   │ 2 hours ago          │
│         │ work            │ hetzner-cloud   │ hel1   │ 3 days ago           │
└─────────┴─────────────────┴─────────────────┴────────┴──────────────────────┘

2 targets configured. Active: 'default'.
```

### 5.3 `apprafter target use <name>`

Switch the active target.

```
$ apprafter target use work
✓ Active target: 'work'
```

### 5.4 `apprafter target show [<name>]`

Detailed view of target's config (secrets masked).

```
$ apprafter target show default

Target: default (active)
  Provider:    hetzner-cloud
  Account:     myname@example.com / project: default
  Region:      nbg1
  SSH key:     ~/.ssh/apprafter_ed25519.pub (loaded)
  Default tier: solo
  Cluster:     not provisioned
  Created:     2026-05-08 14:32:01 UTC
  Last used:   2 hours ago
```

### 5.5 `apprafter target rename <from> <to>`

Rename a target.

### 5.6 `apprafter target remove <name>`

Remove a target (config + credentials).

```
$ apprafter target remove work
? Remove target 'work'? This will delete its credentials and config. › y
✓ Target 'work' removed
```

Skip confirmation with `--yes`.

### 5.7 `apprafter auth ...`

Reserved namespace for AppRafter Cloud (Managed) authentication. Currently a placeholder.

```
$ apprafter auth login
AppRafter Cloud is not yet available.

For self-hosted use, configure a deployment target instead:
    apprafter target add

Track managed availability at: https://apprafter.dev
```

Stubs for:
- `apprafter auth login`
- `apprafter auth logout`
- `apprafter auth status`

These should be hidden from main `--help` output but accessible (so the structure is documented and future-proof). Use `clap`'s `hide = true` for now.

### 5.8 `apprafter whoami`

Combined view of current identity + active target.

```
$ apprafter whoami

Identity:   anonymous (self-hosted mode)
Target:     default (active)
Provider:   hetzner-cloud (verified ✓)
Account:    myname@example.com / project: default
Region:     nbg1
SSH key:    ~/.ssh/id_ed25519.pub (loaded)
Cluster:    not provisioned
Last used:  2 hours ago
```

If credentials are stale (e.g., token revoked): show `verified ✗` with timestamp of last successful check, and hint to run `apprafter target add <name> --renew`.

### 5.9 `apprafter doctor`

Self-diagnostic command. Runs all preflight checks for the active target.

```
$ apprafter doctor

Checking target 'default'...
  ✓ Config file readable
  ✓ Credentials file present (mode 0600)
  ✓ Provider: hetzner-cloud
  ✓ Token format valid
  ✓ Token verified (API ping 142ms)
  ✓ SSH key readable
  ✓ Region 'nbg1' available

Checking environment...
  ✓ kubectl found (v1.31.0)
  ✓ helm found (v3.16.0)
  ✓ DNS resolves api.hetzner.cloud
  ✗ No active cluster (run `apprafter init` to provision)

7 checks passed, 1 warning. Target is ready for `apprafter init`.
```

Each check has clear PASS/FAIL/WARN with miette-style hint for failures.

Useful for:
- New user troubleshooting
- CI smoke checks
- Bug reports (output goes into GitHub issue templates)

### 5.10 `apprafter init`, `apply`, `cluster-bootstrap`

**Keep as separate top-level commands.** Do not merge into a single mega-command. Rationale:
- Each step has different idempotency semantics
- Different steps may need different troubleshooting
- Splitting is more aligned with platform philosophy (decoupled steps, transparent operations)

But each should:
- Auto-resolve credentials from active target (no env vars required by default)
- Show clear progress with `indicatif` spinners/bars
- Fail fast with miette diagnostics if no target is configured
- Support `--target <name>` flag to override active target per command

### 5.11 `apprafter bootstrap-all`

Top-level convenience wrapper that runs the full Tier 1 sequence.

```
$ apprafter bootstrap-all [--target <name>] [--cluster-name <name>]

Phase 1/4: Provisioning infrastructure...
  ✓ Server created (cx22 in nbg1, 142s)
  ✓ Firewall configured
  ✓ Floating IP attached

Phase 2/4: Bootstrapping cluster...
  ✓ k3s installed and ready (38s)
  ✓ Kubeconfig retrieved and encrypted

Phase 3/4: Installing core components...
  ✓ Cilium (network)
  ✓ cert-manager
  ✓ Argo CD

Phase 4/4: Deploying AppRafter operator...
  ✓ CRDs installed
  ✓ Operator deployed and ready
  ✓ Admission webhook active

Total time: 4 minutes 18 seconds
Cluster ready! Access Argo CD at: https://argocd.<your-domain>
```

**Important:** this is **a convenience wrapper**, not a new feature. It just calls existing subcommands in sequence with shared target context. Should be a thin orchestration layer (≤100 LOC).

For phase-by-phase progress, use `indicatif::MultiProgress` to show multiple parallel/sequential bars.

`--dry-run` flag: show what would happen without doing it.

### 5.12 `apprafter version`

Print CLI version, commit hash, and build date. Standard.

---

## 6. Aliases

The following short aliases are registered via `clap`:

- `apprafter t` → `apprafter target`
- `apprafter b` → `apprafter bootstrap-all` (optional, evaluate during implementation)

No alias for `apprafter auth` because `a` would collide with `apply`.

Aliases are documented in `apprafter target --help` (and similar) but not advertised at the top of main help to keep first-time UX simple.

---

## 7. Credential resolution chain

When any subcommand needs a credential (e.g., Hetzner token), the resolution order is:

1. **CLI flag** — `--token "hcloud_..."` (highest priority, explicit override)
2. **Environment variable** — `HCLOUD_TOKEN`, `APPRAFTER_SSH_PUBLIC_KEY` (current behavior, kept for CI/automation compatibility)
3. **Active target's credentials.yaml** — read from `~/.config/apprafter/targets/<active>/credentials.yaml`
4. **Error** — miette diagnostic: "No credentials found. Run `apprafter target add` to set up a target, or pass `--token` flag, or set `HCLOUD_TOKEN` env var."

Resolution happens through a single function:

```rust
pub fn resolve_credential(
    key: CredentialKey,    // e.g., CredentialKey::Hetzner(HetznerCredential::Token)
    cli_override: Option<&str>,
    target: &Target,
) -> miette::Result<Secret>
```

Backwards compatibility: existing env-var-based workflows continue working unchanged. Adding targets is purely additive.

---

## 8. Library and dependency choices

Add the following crates (versions as of latest stable, agent should resolve):

```toml
[dependencies]
# Already present, ensure latest
clap = { version = "4", features = ["derive", "env"] }

# New
inquire = "0.7"            # Interactive prompts (Text, Select, Password, Confirm)
indicatif = "0.17"         # Progress bars and spinners
owo-colors = "4"           # Terminal colors with NO_COLOR support
miette = { version = "7", features = ["fancy"] }   # Diagnostic errors
tabled = "0.15"            # Pretty tables for `target list`, `whoami`
dirs = "5"                 # Cross-platform config/data dirs
serde = { version = "1", features = ["derive"] }   # Already present
serde_yaml = "0.9"         # YAML for config/credentials
secrecy = "0.10"           # Wrapper to prevent accidental Debug-logging of secrets
```

Notes:
- **`inquire` over `dialoguer`** — better validation API, more idiomatic Rust, actively maintained
- **`miette` over `anyhow`** for user-facing errors — anyhow is fine internally, but command boundaries should produce miette `Diagnostic` for nice UX
- **`secrecy::Secret<String>`** for tokens in memory — prevents accidental `println!("{:?}", token)` leaks
- **`tabled` over `comfy-table`** — slightly more flexible derive API

---

## 9. TTY and color handling

### TTY detection

Use `std::io::IsTerminal` (stable since Rust 1.70):

```rust
use std::io::IsTerminal;

let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
```

Behavior:
- **Interactive mode** (TTY detected, no `--no-interactive`): wizards, prompts, progress bars, colors
- **Non-interactive mode** (no TTY or `--no-interactive`): require all inputs via flags/env, plain output, no spinners (or simplified `[step 1/4]` text progress)

### Color policy

Respect (in order):
1. `--color=never|always|auto` flag (highest priority)
2. `NO_COLOR` env var (any non-empty value → no color)
3. `CLICOLOR_FORCE=1` → force color even without TTY
4. TTY detection (default behavior)

`owo-colors` with `supports-colors` feature handles most of this automatically.

---

## 10. Error UX with miette

All user-facing errors must use `miette::Diagnostic`. Example:

```rust
#[derive(thiserror::Error, miette::Diagnostic, Debug)]
pub enum CliError {
    #[error("Invalid Hetzner Cloud API token format")]
    #[diagnostic(
        code(apprafter::target::invalid_token_format),
        help("Hetzner tokens start with 'hcloud_' and have 64+ characters.\nGet a token at: https://console.hetzner.cloud/projects → Security → API Tokens"),
    )]
    InvalidHetznerTokenFormat {
        #[source_code]
        input: String,
        #[label("token must start with `hcloud_`")]
        span: miette::SourceSpan,
    },
    // ... more variants
}
```

This produces the rustc-quality error UX users expect from professional CLI tools.

**Common error patterns to provide:**

- Target not found → list available targets + suggest `apprafter target add`
- Credential missing → show resolution chain + how to provide each
- API call failed → show endpoint, response, suggest `apprafter doctor`
- File permissions wrong → show actual permissions + recommended fix command
- Network unreachable → suggest checking DNS/firewall/VPN
- Stub command `apprafter auth login` → friendly redirect to `apprafter target add`

---

## 11. Validation rules per provider

For now, only Hetzner Cloud is implemented, but the validation framework should be **provider-agnostic** so AWS/etc. can plug in later.

### Hetzner Cloud

| Field | Validation |
|---|---|
| Token | Exactly 64 ASCII alphanumeric chars, no prefix; API ping to `GET /v1/locations` |
| SSH key | File exists, readable, valid OpenSSH public key format |
| Region | Must be in `["nbg1", "fsn1", "hel1", "ash", "hil", "sin"]` (from API) |
| Server type | Must be in API's available types list |

Validation framework signature (rough):

```rust
trait ProviderValidator {
    fn validate_token(&self, token: &Secret<String>) -> miette::Result<()>;
    fn validate_region(&self, region: &str) -> miette::Result<()>;
    fn list_regions(&self) -> miette::Result<Vec<Region>>;
    fn ping(&self, credentials: &ProviderCredentials) -> miette::Result<PingResult>;
}
```

Each provider implements this. The CLI uses it abstractly.

---

## 12. CLI binary rename

In addition to the DX changes, **rename the CLI binary from `platform-cli` to `apprafter`** as part of this work.

Files affected (non-exhaustive):
- `cli/Cargo.toml` — `[[bin]] name = "apprafter"`
- `cli/Cargo.toml` workspace package name
- All `docs/` references
- `plan.md` and `spec.md`
- `Justfile` recipes
- CI workflow files
- Installer/release scripts
- README.md
- Smoke test scripts

Add a thin `platform-cli` shim binary for one release cycle that prints:

```
Warning: `platform-cli` has been renamed to `apprafter`.
Run `apprafter <command>` instead.
This shim will be removed in v0.2.0.
```

And forwards args to `apprafter`. Remove the shim in v0.2.0.

---

## 13. Migration of existing workflows

For users (and the project owner) currently using env-var-based workflow:

**Backwards compatibility:** existing `HCLOUD_TOKEN` and `APPRAFTER_SSH_PUBLIC_KEY` env vars continue to work. They become **step 2** in the resolution chain — they override target credentials but not CLI flags.

**Migration path:**

1. Existing users keep running `HCLOUD_TOKEN=... apprafter init` — unchanged behavior
2. Users opt into targets by running `apprafter target add` — sets up persistent storage
3. After target setup, env vars are no longer needed but still respected if present
4. Future releases may print a soft deprecation note for env-only usage (no immediate removal)

This is **purely additive** — nothing breaks.

---

## 14. Testing requirements

### Unit tests

- Credential resolution chain (mock filesystem + env)
- Target CRUD operations (mock filesystem)
- Token format validation (each provider)
- File permissions enforcement (credentials.yaml must be 0600)
- TTY detection branch coverage
- Color policy resolution

### Integration tests

- Full `apprafter target add` flow with mocked stdin (use `inquire`'s testing utilities)
- `apprafter doctor` against fake API server
- `apprafter bootstrap-all` end-to-end with `FakeRunner` (no real cluster)
- `apprafter auth login` stub correctly redirects to target add

### Smoke tests (opt-in, env-gated as before)

- `APPRAFTER_HETZNER_SMOKE=1` — real Hetzner API ping + target validation against real account
- `APPRAFTER_BOOTSTRAP_SMOKE=1` — full bootstrap-all on real CX22 (existing pattern)

### Manual acceptance criteria

1. Fresh machine: run `apprafter target add`, complete wizard, see target saved
2. Reboot machine, open new terminal: `apprafter whoami` shows target, no env vars needed
3. Run `apprafter doctor`: all checks pass
4. Run `apprafter bootstrap-all`: full Tier 1 cluster ready in < 5 minutes
5. Run `apprafter target add work` for second target, `apprafter target use work` switches
6. Run `apprafter auth login`: friendly redirect message to `apprafter target add`
7. Set `NO_COLOR=1`: no ANSI codes in output
8. Pipe output to file: no progress bars, no colors, no prompts (TTY detection)
9. `apprafter t list` works as alias for `apprafter target list`

---

## 15. Documentation deliverables

The agent should also produce/update:

- `docs/user-guide/cli/targets.md` — target management walkthrough (replaces "login" terminology entirely)
- `docs/user-guide/cli/doctor.md` — troubleshooting
- `docs/user-guide/cli/auth.md` — short stub explaining Managed is upcoming
- `docs/user-guide/quickstart.md` — update with `apprafter target add` → `apprafter bootstrap-all` flow
- Update `README.md` quickstart section
- Update `plan.md` with the new sub-versions used to implement this work
- Add ADR (e.g., `docs/adr/0014-cli-command-structure.md`) documenting:
    - Decision to use resource-first grouping (`target add` not `add target`)
    - Decision to reserve `auth` namespace for future Managed without binding semantics now
    - Decision to use plain YAML over keyring for v1
    - Decision to keep env var compatibility
    - Resolution chain order rationale
    - Multi-target model

---

## 16. Out of scope (explicit non-goals revisited)

These are **not** part of this work, but worth listing to prevent scope creep:

- OS keyring backend (follow-up: `apprafter` v0.1.7x → 0.2.x)
- Age-encrypted credentials (follow-up: if keyring unavailable, age + cached passphrase)
- Team/shared targets (future, Phase 4+ via cloud sync)
- Credential rotation automation (manual `--renew` is the API)
- Multi-cluster per target (one cluster per target for v1; multi-cluster via multiple targets)
- Web-based onboarding (CLI-only)
- GUI configuration tool (Backstage covers the visual layer)
- AppRafter Cloud authentication implementation (only stubs in `apprafter auth`)
- Multi-target deployments from one Application manifest (Application's environments map to ServiceProvider selectors within a single Target; cross-Target deployment is a future concept)

---

## 17. Implementation phasing suggestion

For agent to plan sub-versions:

| Sub-version | Scope |
|---|---|
| v0.1.7x | Rename `platform-cli` → `apprafter`, add shim, update all references |
| v0.1.7x+1 | Target file structure (config/credentials), IO module, no commands yet |
| v0.1.7x+2 | `apprafter target add` non-interactive (flags only) |
| v0.1.7x+3 | `apprafter target add` interactive wizard with inquire |
| v0.1.7x+4 | Validation framework + Hetzner validator + API ping |
| v0.1.7x+5 | `apprafter target list / use / show / rename / remove` |
| v0.1.7x+6 | `apprafter whoami`, `apprafter auth` stubs |
| v0.1.7x+7 | `apprafter doctor` |
| v0.1.7x+8 | Wire existing commands (`init`, `apply`, `cluster-bootstrap`) to target resolution |
| v0.1.7x+9 | `apprafter bootstrap-all` orchestrator with progress UX |
| v0.1.7x+10 | miette error refinement across all commands |
| v0.1.7x+11 | Aliases (`t` → `target`), `--color` flag, `NO_COLOR` support |
| v0.1.7x+12 | Docs + ADR |

Each sub-version: feature + test + docs, releaseable independently.

---

## 18. Acceptance summary

This work is complete when:

- [ ] CLI binary is `apprafter` everywhere; `platform-cli` is a deprecated shim
- [ ] `apprafter target add` interactive wizard works on first run; non-interactive flow works in CI
- [ ] Credentials persist across shells/sessions; no env vars required after `target add`
- [ ] Multi-target management works (`target list/use/rename/remove/show`)
- [ ] `apprafter auth` namespace is reserved (commands exist but stub-redirect to `target add`)
- [ ] `apprafter whoami` shows current state at a glance (identity + target)
- [ ] `apprafter doctor` provides actionable diagnostics
- [ ] `apprafter bootstrap-all` runs full Tier 1 in < 5 minutes with clear progress
- [ ] Aliases work (`apprafter t` → `apprafter target`)
- [ ] All errors use miette with helpful hints
- [ ] `NO_COLOR`, `--color`, `--no-interactive` all work as expected
- [ ] Existing `HCLOUD_TOKEN` workflow still works (backwards compat)
- [ ] Test coverage: unit tests for new modules, integration for new commands, smoke tests updated
- [ ] Docs updated: quickstart, user guides, ADR, plan.md, spec.md
