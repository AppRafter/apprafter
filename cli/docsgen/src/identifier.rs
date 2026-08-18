// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Resolve a documented schema identifier against the shipped schemas.
//!
//! # Why membership, and why page-wide
//!
//! A field-table gate would cover one table in the corpus and three
//! rows. Membership covers those three rows *and* every other place a
//! field name is written: `expose.public` — a field that does not
//! exist — sits in eight locations across three files, six of them
//! fences. Three of those fences are `kubectl apply` YAML, and the
//! apiserver **prunes** an unknown key rather than rejecting it, so a
//! reader who copies the block gets a silent no-op and an
//! apply-and-see check reports success. Membership is the only gate
//! that sees them.
//!
//! # The two surfaces, and why they are extracted differently
//!
//! * **Text** — [`extract_paths`] takes a *backticked* token and only
//!   one whose first component is in [`TEXT_ROOTS`] or [`KIND_ROOTS`].
//!   A backticked `expose.public` in prose or a table cell is a
//!   deliberate reference to our schema, so a strict, closed-root
//!   grammar is enough to separate it from `apprafter.io/hostname`,
//!   `cue.mod`, `bun.lock` and `v0.2.44` — every one of which a broad
//!   dotted-token regex would drag in.
//! * **Structure** — [`extract_structure`] reads a fence body as
//!   nested `key: value` and flattens it. The six `expose.public`
//!   fences write it as YAML/CUE nesting, never as a dotted token, so
//!   without this half the corpus finding the whole check exists for
//!   is unreachable.
//!
//! Structural extraction anchors on the *narrower* [`STRUCTURE_ROOTS`]
//! for a reason that is not arbitrary: in text, a backticked
//! `spec.…` is an explicit claim about the AppRafter schema, but a
//! `spec:` key in a fence is usually somebody else's object — a
//! Deployment, a CiliumNetworkPolicy, an Argo CD `Application`. The
//! five names kept (`base`, `environments`, `expose`, `needs`,
//! `imagePolicy`) do not occur in any of those, so anchoring on them
//! scopes the structural scan to our own manifests without having to
//! first decide what kind a fence declares. `spec`, `metadata`, `env`
//! and `resources` stay text-only.
//!
//! # Reaching prose
//!
//! [`crate::scan`] does not emit plain prose as a block — only fences,
//! spans and front matter. That is not a gap here: every token this
//! module can match is backticked *by construction*, and `scan` lifts
//! every backticked run out of prose into a [`crate::scan::Block`]
//! with `BlockKind::InlineSpan`. So the prose bullet
//! ``- `expose.public`: `true` `` arrives as two spans, and
//! [`span_path`] reads the first. Prose outside a span holds nothing
//! this check could resolve.
//!
//! # What "resolved" does and does not mean
//!
//! **A resolved identifier is not a verified one.** Read the counts
//! with these three limits in view; none of them is a defect to be
//! fixed away, and all three are why the survey reports an opaque
//! ratio next to its total.
//!
//! * **Opaque subtrees — the big one.**
//!   `x-kubernetes-preserve-unknown-fields` and bare `type: object`
//!   nodes describe nothing below themselves, so membership cannot
//!   judge there and [`resolve_path`] passes with [`Verdict::opaque`]
//!   set. That covers `metadata.*`, `env.*`, `expose.hostname.*` and
//!   everything under `needs.<type>`, which is where `size`,
//!   `selector`, `mountPath` and `ref` live. Measured over the
//!   corpus, **95 of 198** resolved identifiers resolve *only* this
//!   way — a shade under half. Nobody should read "198 resolved" as
//!   198 checked, which is exactly what the flag exists to prevent.
//! * **`status` is deliberately not a root.** It is real and the docs
//!   use it twelve times — `status.phase`, `status.availableVersion`,
//!   `status.dbnum`, `status.connectionSecretRef`, `status.refCount`,
//!   `status.versionHistory`, … — and every one of them is
//!   **unchecked** by this module. The reason is the bullet above: the
//!   `status` node of all eight CRDs is preserve-unknown, so admitting
//!   the root would resolve every `status.*` unconditionally and add
//!   twelve to the resolved count while checking nothing. Coverage on
//!   paper is worse than no coverage, because it is read as assurance.
//!   If `status` ever becomes structural in the CRDs, admit the root
//!   then.
//! * **The union can judge a foreign CRD by ours.** Without a kind
//!   prefix there is nothing in a prose span to say whose `spec` it
//!   is, so a bare path is resolved against the union of every
//!   AppRafter kind. Two consequences: a path wrong for the kind a
//!   page is about can resolve against some other kind (a silent
//!   pass), and a *correct* path belonging to a third-party CRD can be
//!   reported. The corpus has one of the latter —
//!   `docs/dev-guide/application-cue.md:28` names Argo CD's
//!   `spec.source.path`, and our `SourceCredential` has a
//!   `spec.source` with no `path`.
//!
//!   **Its disposition is an exemption carrying the typed reason
//!   `external-tool`** ([`crate::marker::Reason::ExternalTool`]),
//!   which is what that reason exists for: a foreign tool's schema,
//!   correctly documented, that our field set cannot and should not
//!   model. It is not drift and the page must not be edited to
//!   "fix" it. Note a kind prefix does **not** help here — the doc is
//!   naming Argo CD's `Application`, not ours.
//!
//! # Prefer the kind-prefixed notation
//!
//! `PlatformStack.spec.pin` is resolved against `PlatformStack` alone,
//! so it has none of the union's silent-pass class; the corpus already
//! writes that form nine times across five distinct paths. Bare paths
//! stay supported because most of the corpus uses them, but a page
//! naming the kind gets a strictly stronger check for free.
//!
//! # Where the field set comes from
//!
//! The committed CRDs under
//! `operator/charts/apprafter-operator/templates/`, not an evaluated
//! `cue`. They are generated from `schemas/v1alpha1` by `crdgen` and
//! byte-compared against it by `just crd-check`, so they are a
//! faithful projection; reading YAML costs nothing at gate time,
//! while shelling out to `cue` would put a subprocess in a
//! pre-commit path. Same reasoning as [`crate::shipped`].
//!
//! Five `schemas/v1alpha1` kinds ship no CRD at all, and the corpus
//! documents one of them — `Infrastructure`, the manifest
//! `apprafter apply` reads — six times. [`FieldSet::from_repo`] adds
//! their `spec` subtree by scanning the CUE as text, for exactly the
//! reason [`crate::shipped`] scans rather than evaluates.

use crate::shipped;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::path::Path;

/// The component a dotted path must open with to be read as a claim
/// about the AppRafter schema at all.
///
/// Closed on purpose. The alternative — any dotted token — matches
/// `apprafter.io/hostname`, `cue.mod`, `bun.lock`, `plan.md` and every
/// version string in the corpus, which is a gate nobody reads.
pub const TEXT_ROOTS: [&str; 9] = [
    "spec",
    "metadata",
    "base",
    "environments",
    "expose",
    "needs",
    "env",
    "resources",
    "imagePolicy",
];

/// The subset of [`TEXT_ROOTS`] a *structural* scan may anchor on: the
/// names that belong to no other Kubernetes object, so finding one as
/// a key means the fence is ours. See the module docs.
pub const STRUCTURE_ROOTS: [&str; 5] = ["base", "environments", "expose", "needs", "imagePolicy"];

/// Kinds a dotted path may open with instead of a field name, so that
/// `PlatformStack.spec.pin` resolves against `PlatformStack` alone.
///
/// This is the notation with no silent-pass hole, and the corpus
/// already uses it six times. A flat table rather than a lookup into
/// the live [`FieldSet`], for the same reason [`crate::shipped`] keeps
/// one: [`extract_paths`] is a pure lexer with no schema in the room,
/// which is what lets its grammar be tested without a repository.
/// `kind_roots_match_the_shipped_schemas` is what keeps it honest.
pub const KIND_ROOTS: [&str; 13] = [
    "AccessGrant",
    "Application",
    "ExternalSurface",
    "Infrastructure",
    "InfrastructureProviderPlugin",
    "MigrationPlan",
    "PlatformStack",
    "ResourceClaim",
    "RetainedClaim",
    "ServiceProvider",
    "ServiceProviderPlugin",
    "SharedVolume",
    "SourceCredential",
];

/// What resolving a path established beyond "it exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// The path names a `needs` key the schema declares but no
    /// provider ships — schema-valid and product-false. See
    /// [`crate::shipped`].
    pub unshipped: bool,
    /// Resolution ended on, or passed through, a node whose subtree
    /// the CRD does not describe — `x-kubernetes-preserve-unknown-fields`,
    /// or a bare `type: object` such as `metadata`. Membership proves
    /// nothing below such a node, so this is reported rather than
    /// quietly folded into a pass: it is the check's known blind spot,
    /// and a caller that wants to know how much of the corpus it
    /// covers has to be able to count it.
    pub opaque: bool,
}

/// One kind's declared field paths.
///
/// `*` stands for one component matched by an `additionalProperties`
/// map — `spec.environments.*.expose.port` is how
/// `environments.prod.expose.port` resolves.
#[derive(Default)]
struct Schema {
    /// Paths with a schema node behind them.
    paths: BTreeSet<String>,
    /// Paths below which nothing can be validated. Anything under one
    /// of these resolves, flagged [`Verdict::opaque`].
    opaque: BTreeSet<String>,
}

/// Every dotted field path the shipped schemas declare, held twice.
///
/// * **Per kind**, which is how a documented `PlatformStack.spec.pin`
///   is resolved: against `PlatformStack` and nothing else.
/// * **As a flat union**, which is how a bare `spec.pin` is resolved,
///   because nothing in a prose span says whose `spec` it is.
///
/// The union is the imprecise half and is kept only because most of
/// the corpus writes the bare form. Its cost is a silent pass: a path
/// that is wrong for the kind the page is about can still resolve
/// against some *other* kind that happens to declare it. The
/// kind-prefixed notation has no such hole, which is why the grammar
/// admits it ([`KIND_ROOTS`]) and why it is the form to prefer when
/// writing docs.
pub struct FieldSet {
    union: Schema,
    kinds: BTreeMap<String, Schema>,
}

impl FieldSet {
    /// Read every `crd-*.yaml` in a chart templates directory.
    ///
    /// Loud rather than quiet on trouble: no CRD files, or a CRD with
    /// no `openAPIV3Schema`, is an error and not an empty set. An
    /// empty set would resolve nothing and *fail* the whole corpus,
    /// which at least is not silent — but it would fail it for the
    /// wrong reason, and the message here says which file is at fault.
    pub fn from_crds(templates_dir: &Path) -> Result<FieldSet, Box<dyn Error>> {
        let mut files: Vec<_> = std::fs::read_dir(templates_dir)
            .map_err(|e| format!("reading {}: {e}", templates_dir.display()))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.starts_with("crd-") && name.ends_with(".yaml")
            })
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(format!(
                "no crd-*.yaml under {} — the chart layout moved and this \
                 check would silently resolve nothing",
                templates_dir.display()
            )
            .into());
        }

        let mut set = FieldSet {
            union: Schema::default(),
            kinds: BTreeMap::new(),
        };
        for file in &files {
            let raw = std::fs::read_to_string(file)
                .map_err(|e| format!("reading {}: {e}", file.display()))?;
            let doc: serde_yaml::Value = serde_yaml::from_str(&without_helm_actions(&raw))
                .map_err(|e| format!("parsing {}: {e}", file.display()))?;
            let spec = doc
                .get("spec")
                .ok_or_else(|| format!("{}: no spec", file.display()))?;
            // The kind is what a doc page writes as a prefix, so read it
            // from the CRD rather than inferring it from the filename.
            let kind = spec
                .get("names")
                .and_then(|names| names.get("kind"))
                .and_then(|kind| kind.as_str())
                .ok_or_else(|| format!("{}: no spec.names.kind", file.display()))?
                .to_string();
            let versions = spec
                .get("versions")
                .and_then(|versions| versions.as_sequence())
                .ok_or_else(|| format!("{}: no spec.versions", file.display()))?;
            let mut own = Schema::default();
            let mut found = false;
            for version in versions {
                if let Some(schema) = version.get("schema").and_then(|s| s.get("openAPIV3Schema")) {
                    walk(schema, "", &mut own);
                    found = true;
                }
            }
            if !found {
                return Err(format!(
                    "{}: no version carries a schema.openAPIV3Schema",
                    file.display()
                )
                .into());
            }
            set.absorb(&kind, own);
        }
        Ok(set)
    }

    /// Fold one kind's schema into both halves of the set.
    fn absorb(&mut self, kind: &str, own: Schema) {
        self.union.paths.extend(own.paths.iter().cloned());
        self.union.opaque.extend(own.opaque.iter().cloned());
        let slot = self.kinds.entry(kind.to_string()).or_default();
        slot.paths.extend(own.paths);
        slot.opaque.extend(own.opaque);
    }

    /// The whole shipped `v1alpha1` surface: the CRDs, plus the `spec`
    /// subtree of every schema in `schemas/v1alpha1` that ships **no**
    /// CRD.
    ///
    /// Five schemas are CUE-only — `Infrastructure`,
    /// `InfrastructureProviderPlugin`, `ServiceProviderPlugin`,
    /// `AccessGrant`, `ExternalSurface`. `Infrastructure` is not an
    /// obscure corner: it is the manifest `apprafter apply` reads, and
    /// the operator guides name `spec.nodes` and
    /// `spec.argocd.bootstrapRepo` six times between them. Resolving
    /// only against CRDs calls all six wrong, and a gate that fails
    /// correct documentation is a gate that gets switched off.
    ///
    /// Only the `spec` subtree is taken: `apiVersion`, `kind` and
    /// `metadata` are the same shape on every kind and the CRDs
    /// already carry them.
    pub fn from_repo(repo_root: &Path) -> Result<FieldSet, Box<dyn Error>> {
        let mut set =
            FieldSet::from_crds(&repo_root.join("operator/charts/apprafter-operator/templates"))?;
        let schemas = repo_root.join("schemas/v1alpha1");
        let templates = repo_root.join("operator/charts/apprafter-operator/templates");
        for entry in std::fs::read_dir(&schemas)
            .map_err(|e| format!("reading {}: {e}", schemas.display()))?
        {
            let path = entry?.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().and_then(|e| e.to_str()) != Some("cue") {
                continue;
            }
            if templates.join(format!("crd-{stem}.yaml")).exists() {
                continue;
            }
            let source = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            let Some(kind) = declared_kind(&source) else {
                // `types.cue` holds `#TypeMeta` / `#ObjectMeta` and
                // declares no kind of its own. Nothing to attribute.
                continue;
            };
            let mut own = Schema::default();
            for (_, found) in flatten(&source) {
                if found == "spec" || found.starts_with("spec.") {
                    own.paths.insert(found);
                }
            }
            set.absorb(&kind, own);
        }
        Ok(set)
    }

    /// Every kind this set can resolve a prefix against, sorted.
    /// [`KIND_ROOTS`] is pinned against it by test.
    pub fn kinds(&self) -> Vec<&str> {
        self.kinds.keys().map(String::as_str).collect()
    }

    /// How many concrete paths were read. Only a sanity handle for
    /// tests — a set that silently shrank to a handful would otherwise
    /// look like a working gate.
    pub fn len(&self) -> usize {
        self.union.paths.len()
    }

    /// Whether the set is empty, which is always a bug here.
    pub fn is_empty(&self) -> bool {
        self.union.paths.is_empty()
    }
}

/// Drop Helm action lines before handing a chart template to a YAML
/// parser.
///
/// The CRDs are templates only in their `metadata.labels`, which is a
/// single `{{- include … }}` line. Removing it leaves `labels:` with a
/// null value — valid YAML — and touches nothing under
/// `openAPIV3Schema`, the only part this module reads.
fn without_helm_actions(raw: &str) -> String {
    raw.lines()
        .filter(|line| !line.trim_start().starts_with("{{"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Record one schema node and recurse into everything it declares.
fn walk(node: &serde_yaml::Value, prefix: &str, set: &mut Schema) {
    let Some(map) = node.as_mapping() else {
        return;
    };
    let get = |key: &str| map.get(serde_yaml::Value::from(key));

    let preserves = get("x-kubernetes-preserve-unknown-fields")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut described = false;

    if let Some(properties) = get("properties").and_then(|p| p.as_mapping()) {
        described = true;
        for (name, child) in properties {
            let Some(name) = name.as_str() else { continue };
            let path = join(prefix, name);
            set.paths.insert(path.clone());
            walk(child, &path, set);
        }
    }

    // A map: one wildcard component standing for any key.
    if let Some(additional) = get("additionalProperties") {
        if additional.as_mapping().is_some() {
            described = true;
            let path = join(prefix, "*");
            set.paths.insert(path.clone());
            walk(additional, &path, set);
        } else if additional.as_bool() == Some(true) {
            set.opaque.insert(prefix.to_string());
        }
    }

    // An array contributes no component to a dotted path — a reader
    // writes `spec.steps.action`, not `spec.steps[0].action` — so its
    // items are walked at the same prefix.
    if let Some(items) = get("items") {
        described = true;
        walk(items, prefix, set);
    }

    let scalar = matches!(
        get("type").and_then(|t| t.as_str()),
        Some("string" | "integer" | "number" | "boolean")
    );

    // Unvalidatable below this point: an explicit preserve-unknown, or
    // a node that declares no shape at all (`metadata: {type: object}`,
    // and every `needs.<type>` leaf).
    if preserves || (!described && !scalar) {
        set.opaque.insert(prefix.to_string());
    }
}

/// The `kind: "X"` a CUE schema declares, by scan.
///
/// The kind is a literal in every one of these files, so reading it is
/// exact — no inference from the filename, which would get
/// `infrastructureproviderplugin.cue` wrong.
fn declared_kind(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("kind:") else {
            continue;
        };
        let value = rest.trim();
        if let Some(name) = value.strip_prefix('"').and_then(|v| v.split('"').next()) {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

/// Every backticked token in `text` that reads as a schema path.
///
/// Anchored: the *whole* span must be the path. A span like
/// `` `expose.network: "public"` `` is a sentence about a field, not a
/// reference to one, and splitting spans into words to reach it would
/// admit every shell word and YAML fragment that shares a span with a
/// real token.
pub fn extract_paths(text: &str) -> Vec<String> {
    spans(text)
        .into_iter()
        .filter(|token| is_schema_path(token))
        .collect()
}

/// The schema path an already-unwrapped inline span carries, if any.
///
/// [`crate::scan`] hands back a span's *content*, backticks removed.
/// Re-wrapping it to feed [`extract_paths`] would misparse the eleven
/// corpus spans that themselves contain a backtick, so the span
/// surface calls this instead.
pub fn span_path(body: &str) -> Option<String> {
    let token = body.trim();
    is_schema_path(token).then(|| token.to_string())
}

/// File suffixes that make a two-component token a *filename* rather
/// than a field path.
///
/// One root — `spec` — is also the stem of a file the docs cite
/// constantly, so `spec.md` matches the grammar exactly and appears
/// twelve times in the corpus. `plan.md` and `cue.mod` are already out
/// because their stems are not roots; this is the same rule reaching
/// the one case that shares a stem. Deliberately excludes `env`, which
/// is a real field (`base.env`) before it is an extension.
const FILE_SUFFIXES: [&str; 22] = [
    "md", "cue", "yaml", "yml", "json", "toml", "lock", "rs", "go", "ts", "js", "sh", "txt", "tpl",
    "tf", "pem", "crt", "pub", "log", "ini", "sum", "mod",
];

/// `^<root>(\.[a-zA-Z][a-zA-Z0-9]*)+$` with `<root>` from
/// [`TEXT_ROOTS`], spelled out rather than compiled — the crate has no
/// regex dependency and this is the whole grammar.
fn is_schema_path(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    let Some((root, rest)) = parts.split_first() else {
        return false;
    };
    if !TEXT_ROOTS.contains(root) && !KIND_ROOTS.contains(root) {
        return false;
    }
    if rest.is_empty() {
        return false;
    }
    // `Application.cue` and `Infrastructure.cue` are files, and both
    // stems are kinds — the same collision `spec.md` has, one level up.
    if rest.len() == 1 && FILE_SUFFIXES.contains(&rest[0]) {
        return false;
    }
    rest.iter().all(|part| {
        let mut chars = part.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric())
    })
}

/// Contents of every code span in `text`: a run of N backticks opens,
/// the next run of exactly N closes. Mirrors [`crate::scan`]'s rule so
/// the two surfaces agree on what a span is.
fn spans(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'`' {
            at += 1;
            continue;
        }
        let mut after = at;
        while after < bytes.len() && bytes[after] == b'`' {
            after += 1;
        }
        let width = after - at;
        let mut scan = after;
        let mut closed = None;
        while scan < bytes.len() {
            if bytes[scan] != b'`' {
                scan += 1;
                continue;
            }
            let start = scan;
            while scan < bytes.len() && bytes[scan] == b'`' {
                scan += 1;
            }
            if scan - start == width {
                closed = Some((start, scan));
                break;
            }
        }
        match closed {
            Some((from, to)) => {
                out.push(text[after..from].trim().to_string());
                at = to;
            }
            None => at = after,
        }
    }
    out
}

// ---- structural extraction --------------------------------------------

/// One key path found by flattening a fence body, with the 0-based
/// line offset inside that body it was written on.
pub type Located = (usize, String);

/// Flatten a fence body's `key: value` nesting into dotted paths,
/// keeping the ones anchored at a [`STRUCTURE_ROOTS`] component.
///
/// A hand-rolled scanner rather than `serde_yaml`, for three reasons
/// the corpus forces: half the manifests are **CUE**, not YAML
/// (`spec: base: { … }` chains, `&` unification, `#Application`
/// references); several fences are a *fragment*, not a document; and
/// three are inside a `kubectl apply -f - <<'YAML'` heredoc, so the
/// block also holds shell. A parser rejects all three; a scanner reads
/// the keys out of each and ignores what it does not understand.
pub fn extract_structure(body: &str) -> Vec<Located> {
    flatten(body)
        .into_iter()
        .filter_map(|(line, path)| {
            let parts: Vec<&str> = path.split('.').collect();
            parts
                .iter()
                .position(|p| STRUCTURE_ROOTS.contains(p))
                .map(|from| (line, parts[from..].join(".")))
        })
        .collect()
}

/// Every key path in `body`, from its own outermost key down — the
/// unanchored form [`extract_structure`] filters and
/// [`FieldSet::from_repo`] harvests a CUE schema with.
fn flatten(body: &str) -> Vec<Located> {
    let mut out = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    for (index, raw) in body.lines().enumerate() {
        let line = strip_comment(raw);
        if line.trim().is_empty() {
            continue;
        }
        let (indent, rest) = strip_list_marker(&line);
        while matches!(stack.last(), Some(f) if matches!(f.kind, Kind::Indented(i) if i >= indent))
        {
            stack.pop();
        }
        scan_line(rest, indent, index, &mut stack, &mut out);
    }
    out
}

/// A nesting level. Braced and bracketed levels are closed by their own
/// delimiter; indented ones by the first line at or left of their own
/// column.
struct Frame {
    /// More than one when a CUE line chains — `spec: base: {` opens two
    /// components that one `}` closes.
    keys: Vec<String>,
    kind: Kind,
}

#[derive(PartialEq, Eq)]
enum Kind {
    Braced,
    /// A sequence. It contributes **no** component of its own: a reader
    /// writes `needs.pg.name`, not `needs.pg[0].name`. Carrying the key
    /// across the bracket is what the corpus forces — `pg: [ { name:
    /// … } ]` is how every named multi-claim is documented, and
    /// dropping `pg` there turned eleven correct lines into findings.
    Bracketed,
    Indented(usize),
}

/// Drop a trailing `//` (CUE) or `#` (YAML) comment, respecting quotes
/// so a `#` inside a string survives. `#` opens a comment only when the
/// next character is not an identifier one, which is what keeps CUE's
/// `#Application` from reading as a comment.
fn strip_comment(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'"' | b'\'' => at = skip_quoted(bytes, at),
            b'/' if bytes.get(at + 1) == Some(&b'/') => return line[..at].to_string(),
            b'#' if !matches!(bytes.get(at + 1), Some(c) if c.is_ascii_alphanumeric() || *c == b'_') => {
                return line[..at].to_string()
            }
            _ => at += 1,
        }
    }
    line.to_string()
}

/// Leading whitespace width, with a `- ` list marker counted as two
/// columns of it so a sequence entry's keys nest where they look like
/// they nest.
fn strip_list_marker(line: &str) -> (usize, &str) {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let indent = line.len() - trimmed.len();
    match trimmed.strip_prefix("- ") {
        Some(rest) => (indent + 2, rest),
        None => (indent, trimmed),
    }
}

/// Read one line's keys and values into `stack` / `out`.
fn scan_line(
    line: &str,
    indent: usize,
    index: usize,
    stack: &mut Vec<Frame>,
    out: &mut Vec<Located>,
) {
    let bytes = line.as_bytes();
    let mut at = 0;
    // Keys read on this line that have not met their value yet.
    let mut pending: Vec<String> = Vec::new();
    while at < bytes.len() {
        match bytes[at] {
            b' ' | b'\t' => at += 1,
            b'{' | b'[' => {
                emit(stack, &pending, index, out);
                stack.push(Frame {
                    keys: std::mem::take(&mut pending),
                    kind: if bytes[at] == b'{' {
                        Kind::Braced
                    } else {
                        Kind::Bracketed
                    },
                });
                at += 1;
            }
            b'}' | b']' => {
                pending.clear();
                let closes = if bytes[at] == b'}' {
                    Kind::Braced
                } else {
                    Kind::Bracketed
                };
                // Only if there is something to close. A shell fence is
                // full of `${VAR}` and `[ -f x ]`, and letting either
                // unwind the stack would put the rest of the block at
                // the wrong depth.
                if stack.iter().any(|frame| frame.kind == closes) {
                    // Give up any indented levels trapped inside the
                    // delimiter first, so one closer cannot pop a
                    // sibling.
                    while let Some(frame) = stack.pop() {
                        if frame.kind == closes {
                            break;
                        }
                    }
                }
                at += 1;
            }
            b',' => {
                pending.clear();
                at += 1;
            }
            b'"' | b'\'' => {
                let end = skip_quoted(bytes, at);
                let text = line[at + 1..end.saturating_sub(1).max(at + 1)].to_string();
                at = end;
                match key_separator(bytes, at) {
                    Some(next) => {
                        pending.push(text);
                        at = next;
                    }
                    None => {
                        emit(stack, &pending, index, out);
                        pending.clear();
                        at = skip_value(bytes, at);
                    }
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = at;
                while at < bytes.len()
                    && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] == b'-')
                {
                    at += 1;
                }
                let word = line[start..at].to_string();
                match key_separator(bytes, at) {
                    Some(next) => {
                        at = next;
                        pending.push(word);
                        if line[at..].trim().is_empty() {
                            // `expose:` at end of line — an indented block.
                            emit(stack, &pending, index, out);
                            stack.push(Frame {
                                keys: std::mem::take(&mut pending),
                                kind: Kind::Indented(indent),
                            });
                        }
                    }
                    None => {
                        emit(stack, &pending, index, out);
                        pending.clear();
                        at = skip_value(bytes, at);
                    }
                }
            }
            _ => {
                emit(stack, &pending, index, out);
                pending.clear();
                at = skip_value(bytes, at);
            }
        }
    }
}

/// Offset just past a `:` (or `?:`) that separates a key from a value,
/// if one starts at `at`.
///
/// The colon must be followed by whitespace, end of line, or `{`. That
/// single condition is what keeps an unquoted image reference —
/// `image: nginxdemos/hello:plain-text`, which is real in the corpus —
/// from reading its own tag as a nested key.
fn key_separator(bytes: &[u8], at: usize) -> Option<usize> {
    let mut scan = at;
    while matches!(bytes.get(scan), Some(b' ' | b'\t')) {
        scan += 1;
    }
    if bytes.get(scan) == Some(&b'?') {
        scan += 1;
    }
    if bytes.get(scan) != Some(&b':') {
        return None;
    }
    scan += 1;
    match bytes.get(scan) {
        None | Some(b' ' | b'\t' | b'{' | b'[') => Some(scan),
        _ => None,
    }
}

/// Skip a scalar value, stopping where structure resumes.
fn skip_value(bytes: &[u8], at: usize) -> usize {
    let mut scan = at;
    while scan < bytes.len() {
        match bytes[scan] {
            b',' | b'{' | b'}' | b'[' | b']' => return scan,
            b'"' | b'\'' => scan = skip_quoted(bytes, scan),
            _ => scan += 1,
        }
    }
    scan
}

/// Offset just past the string opening at `at`.
fn skip_quoted(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    let mut scan = at + 1;
    while scan < bytes.len() {
        if bytes[scan] == b'\\' {
            scan += 2;
            continue;
        }
        if bytes[scan] == quote {
            return scan + 1;
        }
        scan += 1;
    }
    scan
}

/// Emit the path for `keys` under `stack`, and for each of its
/// prefixes — a chained `spec: base: image: "x"` names three nodes,
/// and an interior typo is drift just as much as a leaf one.
fn emit(stack: &[Frame], keys: &[String], index: usize, out: &mut Vec<Located>) {
    if keys.is_empty() {
        return;
    }
    let mut parts: Vec<&str> = stack
        .iter()
        .flat_map(|frame| frame.keys.iter())
        .map(String::as_str)
        .collect();
    for key in keys {
        parts.push(key);
        out.push((index, parts.join(".")));
    }
}

// ---- resolution -------------------------------------------------------

/// Resolve a documented path against the shipped schemas.
///
/// The path may be written from any depth: docs say `expose.port` and
/// `spec.base.expose.port` for the same field, so a query matches as a
/// **component-aligned suffix** of a schema path. Alignment is what
/// keeps that from becoming "contains": a Deployment's `spec.replicas`
/// does not resolve, because no schema path ends with those two
/// components in that order even though `spec` and `replicas` both
/// occur.
///
/// A leading **kind** narrows the search to that kind alone —
/// `PlatformStack.spec.pin` is resolved against `PlatformStack` and
/// nothing else. That is what removes the union's silent-pass class,
/// where a path wrong for the kind a page is about resolves anyway
/// because some other kind declares it. Without a kind prefix the
/// union is all there is: a prose span does not say whose `spec` it is.
pub fn resolve_path(fields: &FieldSet, path: &str) -> Result<Verdict, String> {
    let mut query: Vec<&str> = path.split('.').collect();
    let mut schema = &fields.union;
    let mut kind = None;
    if let Some(named) = fields.kinds.get(query[0]) {
        // A kind alone is not a field path; `Application` on its own
        // never reaches here because the grammar demands a `.`.
        if query.len() > 1 {
            kind = Some(query.remove(0));
            schema = named;
        }
    }
    let unshipped = declared_but_unshipped(&query);

    if let Some(matched) = schema.paths.iter().find(|c| suffix_of(c, &query)) {
        return Ok(Verdict {
            unshipped,
            opaque: schema.opaque.contains(matched.as_str())
                || schema.opaque.iter().any(|o| suffix_of(o, &query)),
        });
    }

    // Nothing describes what is below an opaque node, so anything under
    // one has to pass — flagged, not hidden.
    for depth in (1..query.len()).rev() {
        if schema.opaque.iter().any(|o| suffix_of(o, &query[..depth])) {
            return Ok(Verdict {
                unshipped,
                opaque: true,
            });
        }
    }

    Err(explain(schema, kind, &query))
}

/// Whether `candidate` (a stored CRD path, `*` matching any single
/// component) ends with exactly `query`.
///
/// The query's **first** component may not land on a `*`. Without that
/// rule `expose` matches `spec.base.env.*` — the wildcard standing for
/// "any env-var name" happily absorbs it — and with it every drifted
/// path in the corpus resolves through some map. A documented path
/// always opens on a field *name*; only its later components can be a
/// map key.
fn suffix_of(candidate: &str, query: &[&str]) -> bool {
    let parts: Vec<&str> = candidate.split('.').collect();
    if parts.len() < query.len() {
        return false;
    }
    let tail = &parts[parts.len() - query.len()..];
    tail[0] != "*"
        && tail
            .iter()
            .zip(query)
            .all(|(stored, asked)| *stored == "*" || stored == asked)
}

/// `Declared` in `#Needs` but with no provider behind it — the second
/// gate. The component after a `needs` one names the type.
fn declared_but_unshipped(query: &[&str]) -> bool {
    query
        .windows(2)
        .filter(|pair| pair[0] == "needs")
        .any(|pair| shipped::status(pair[1]) == Some(shipped::Status::Declared))
}

/// Name the component that failed and what the schema offers instead.
fn explain(schema: &Schema, kind: Option<&str>, query: &[&str]) -> String {
    let written = match kind {
        Some(kind) => format!("{kind}.{}", query.join(".")),
        None => query.join("."),
    };
    // Which schema said so — without it, a kind-scoped rejection reads
    // as if no kind at all declared the field.
    let scope = match kind {
        Some(kind) => format!(" on `{kind}`"),
        None => String::new(),
    };
    for depth in (1..query.len()).rev() {
        let prefix = &query[..depth];
        if !schema.paths.iter().any(|c| suffix_of(c, prefix)) {
            continue;
        }
        let mut known: Vec<&str> = schema
            .paths
            .iter()
            .filter_map(|candidate| {
                let parts: Vec<&str> = candidate.split('.').collect();
                (parts.len() > prefix.len() && suffix_of(candidate, &parts_tail(&parts, prefix)))
                    .then(|| *parts.last().expect("non-empty"))
            })
            .collect();
        known.sort_unstable();
        known.dedup();
        let offered = if known.is_empty() {
            "it has no declared fields".to_string()
        } else {
            format!("`{}` has {}", prefix.join("."), known.join(", "))
        };
        return format!(
            "`{written}`: no such field — `{}` is not under `{}`{scope} ({offered})",
            query[depth],
            prefix.join(".")
        );
    }
    format!("`{written}`: no schema{scope} declares a field path ending in it")
}

/// The prefix extended by `parts`' last component — used to ask "is
/// this candidate a direct child of `prefix`".
fn parts_tail<'a>(parts: &'a [&'a str], prefix: &'a [&'a str]) -> Vec<&'a str> {
    let mut tail: Vec<&str> = prefix.to_vec();
    tail.push(parts.last().expect("non-empty"));
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> FieldSet {
        FieldSet::from_crds(
            &crate::repo_root()
                .unwrap()
                .join("operator/charts/apprafter-operator/templates"),
        )
        .unwrap()
    }

    #[test]
    fn the_crds_yield_a_populated_field_set() {
        let f = fields();
        assert!(
            f.len() > 100,
            "only {} paths — the walk stopped early",
            f.len()
        );
        assert!(!f.is_empty());
    }

    #[test]
    fn a_wildcard_component_matches_any_environment_name() {
        let f = fields();
        resolve_path(&f, "environments.prod.expose.port").expect("per-env override");
        resolve_path(&f, "spec.environments.dev.replicas").expect("per-env override");
    }

    #[test]
    fn an_unaligned_suffix_does_not_resolve() {
        // `spec` and `replicas` both exist; `spec.replicas` (a
        // Deployment's field) does not, and must not borrow them.
        let f = fields();
        assert!(resolve_path(&f, "spec.replicas").is_err());
        assert!(resolve_path(&f, "expose.replicas").is_err());
    }

    #[test]
    fn an_opaque_subtree_passes_and_says_so() {
        let f = fields();
        // `metadata` is `type: object` with no properties; `needs.pg` and
        // `env.*` are preserve-unknown. Membership cannot judge below
        // any of them.
        for path in ["metadata.name", "needs.pg.size", "env.LOG"] {
            let verdict = resolve_path(&f, path).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert!(verdict.opaque, "{path} must be reported as opaque");
        }
    }

    #[test]
    fn structure_reads_cue_chains_yaml_nesting_and_flow_maps() {
        // All three shapes are real corpus fences.
        let cue =
            "spec: base: {\n    expose: {\n        port:   8080\n        public: false\n    }\n}\n";
        assert!(extract_structure(cue)
            .iter()
            .any(|(_, p)| p == "base.expose.public"));
        assert!(extract_structure(cue)
            .iter()
            .any(|(_, p)| p == "base.expose.port"));

        let yaml = "spec:\n  base:\n    expose:\n      port: 80\n      public: false\n";
        assert!(extract_structure(yaml)
            .iter()
            .any(|(_, p)| p == "base.expose.public"));

        let flow = "    expose: { port: 80, public: false }\n";
        assert!(extract_structure(flow)
            .iter()
            .any(|(_, p)| p == "expose.public"));
    }

    #[test]
    fn an_unquoted_image_tag_is_not_a_nested_key() {
        // `hello:plain-text` must not become `base.image.hello`.
        let yaml = "spec:\n  base:\n    image: nginxdemos/hello:plain-text\n";
        let found = extract_structure(yaml);
        assert!(
            found.iter().all(|(_, p)| p != "base.image.hello"),
            "{found:?}"
        );
    }

    #[test]
    fn a_line_number_is_carried() {
        let yaml = "spec:\n  base:\n    expose:\n      public: false\n";
        let hit = extract_structure(yaml)
            .into_iter()
            .find(|(_, p)| p == "base.expose.public")
            .expect("found");
        assert_eq!(hit.0, 3, "0-based offset of the `public:` line");
    }

    #[test]
    fn a_span_body_is_read_without_re_wrapping() {
        assert_eq!(span_path("expose.public").as_deref(), Some("expose.public"));
        assert_eq!(span_path("apprafter app list"), None);
    }
}
