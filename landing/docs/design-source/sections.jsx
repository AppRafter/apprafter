/* eslint-disable */
// AppRafter landing — sections.
// All copy is locked-in from LANDING_BRIEF v2.2 (English website strings).
// Schema in hero snippet matches v1alpha1 (no needs/claim — those land in 2.x).

// ============================================================
// Header
// ============================================================
const Header = ({ theme, onToggleTheme, logoVariant }) => {
  const [scrolled, setScrolled] = React.useState(false);
  React.useEffect(() => {
    const onScroll = () => setScrolled(window.scrollY > 8);
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  return (
    <header className={"site-header" + (scrolled ? " scrolled" : "")} id="top">
      <div className="container row">
        <Brand variant={logoVariant} size={26} />
        <nav className="nav" aria-label="Primary">
          <a href="https://github.com/AppRafter/apprafter#readme" target="_blank" rel="noreferrer" className="soon">Docs</a>
          <a href="https://github.com/AppRafter/apprafter/blob/main/spec.md" target="_blank" rel="noreferrer">Spec</a>
          <a href="https://github.com/AppRafter/apprafter" target="_blank" rel="noreferrer" aria-label="GitHub">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M12 .5A12 12 0 0 0 0 12.5a12 12 0 0 0 8.2 11.4c.6.1.8-.3.8-.6v-2.1c-3.3.7-4-1.6-4-1.6-.6-1.4-1.4-1.8-1.4-1.8-1.1-.7.1-.7.1-.7 1.2.1 1.9 1.3 1.9 1.3 1.1 1.9 2.9 1.4 3.7 1 .1-.8.4-1.4.8-1.7-2.6-.3-5.4-1.3-5.4-6 0-1.3.5-2.4 1.2-3.2-.1-.3-.5-1.6.1-3.3 0 0 1-.3 3.3 1.2a11.4 11.4 0 0 1 6 0c2.3-1.5 3.3-1.2 3.3-1.2.6 1.7.2 3 .1 3.3.8.8 1.2 1.9 1.2 3.2 0 4.7-2.8 5.7-5.5 6 .4.4.8 1.1.8 2.3v3.3c0 .3.2.7.8.6A12 12 0 0 0 24 12.5 12 12 0 0 0 12 .5z" />
            </svg>
          </a>
          <button
            className="icon-btn"
            onClick={onToggleTheme}
            aria-label={theme === "dark" ? "Switch to light theme" : "Switch to dark theme"}
            title={theme === "dark" ? "Light theme" : "Dark theme"}
          >
            {theme === "dark" ? (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <circle cx="12" cy="12" r="4" />
                <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
              </svg>
            ) : (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z" />
              </svg>
            )}
          </button>
        </nav>
      </div>
    </header>
  );
};

// ============================================================
// Hero
// ============================================================
const CUE_SNIPPET = `apiVersion: apprafter.io/v1alpha1
kind: Application
metadata: {
    name:      "billing-api"
    namespace: "prod"
}

spec: {
    base: {
        image:    "ghcr.io/me/billing:v1.4.2"
        replicas: 3

        expose: {
            port:    8080
            public:  true
            network: "public"
        }

        needs: {
            pg: {size: "small"}
        }

        env: {
            DATABASE_URL: from: claim.pg.uri
            LOG_LEVEL:    "info"
        }
    }

    environments: {
        dev:  base & {replicas: 1, expose: network: "vpn"}
        prod: base & {replicas: 3}
    }
}`;

// Very small CUE-flavoured tokenizer — purely cosmetic, no real parsing.
const renderCue = (src) => {
  // Split into lines, tokenize each line.
  return src.split("\n").map((line, i) => {
    const tokens = [];
    let rest = line;
    // Leading whitespace
    const leadMatch = rest.match(/^(\s+)/);
    if (leadMatch) { tokens.push(leadMatch[1]); rest = rest.slice(leadMatch[1].length); }

    // Comment
    if (rest.startsWith("//")) {
      tokens.push(<span className="tok-cmt" key="c">{rest}</span>);
      return <div key={i}>{tokens}</div>;
    }

    // Process the line — naive but readable.
    const re = /("(?:[^"\\]|\\.)*")|(\b\d+\b)|(\b(?:apiVersion|kind|metadata|spec|base|environments|expose|env|image|replicas|port|public|network|name|namespace|needs|from|claim|size)\b)|(&|\||\?|:|\.|\{|\}|\[|\])|([A-Za-z_][A-Za-z0-9_-]*)/g;
    let m;
    let last = 0;
    while ((m = re.exec(rest)) !== null) {
      if (m.index > last) tokens.push(rest.slice(last, m.index));
      if (m[1]) tokens.push(<span className="tok-str" key={`s${i}-${m.index}`}>{m[1]}</span>);
      else if (m[2]) tokens.push(<span className="tok-num" key={`n${i}-${m.index}`}>{m[2]}</span>);
      else if (m[3]) tokens.push(<span className="tok-key" key={`k${i}-${m.index}`}>{m[3]}</span>);
      else if (m[4]) tokens.push(<span className="tok-kw" key={`p${i}-${m.index}`}>{m[4]}</span>);
      else if (m[5]) tokens.push(<span className="tok-ident" key={`i${i}-${m.index}`}>{m[5]}</span>);
      last = m.index + m[0].length;
    }
    if (last < rest.length) tokens.push(rest.slice(last));
    return <div key={i} style={{ minHeight: "1.65em" }}>{tokens.length ? tokens : "\u00A0"}</div>;
  });
};

const HeroSnippet = () => {
  const [copied, setCopied] = React.useState(false);
  const copy = () => {
    navigator.clipboard.writeText(CUE_SNIPPET).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }).catch(() => {});
  };
  return (
    <div className="codeblock" aria-label="Application manifest example">
      <div className="codeblock-header">
        <span className="filename">billing-api.cue</span>
        <button className={"copy-btn" + (copied ? " copied" : "")} onClick={copy} aria-live="polite">
          {copied ? (
            <>
              <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="20 6 9 17 4 12" />
              </svg>
              Copied
            </>
          ) : (
            <>
              <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <rect x="9" y="9" width="13" height="13" rx="1" />
                <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
              </svg>
              Copy
            </>
          )}
        </button>
      </div>
      <pre>{renderCue(CUE_SNIPPET)}</pre>
    </div>
  );
};

const Hero = ({ onOpenWaitlist, waitlistOpen, version }) => {
  return (
    <section className="hero" aria-label="Hero">
      <div className="container hero-grid">
        <div>
          <h1>
            One manifest. From a <span className="accented">€5 VPS</span> to production. Open source.
          </h1>
          <p className="subhead">
            AppRafter is an opinionated PaaS on Kubernetes. Describe your applications in a single CUE manifest — the same one runs from a single VDS to a multi-node production cluster. Open source (FSL-1.1-Apache-2.0). A managed version is coming for those who'd rather not run ops themselves.
          </p>
          <div className="ctas">
            <a className="btn btn-primary" href="https://github.com/AppRafter/apprafter#quickstart" target="_blank" rel="noreferrer">
              Try self-host
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <line x1="5" y1="12" x2="19" y2="12" />
                <polyline points="12 5 19 12 12 19" />
              </svg>
            </a>
            <button
              className="btn btn-secondary"
              onClick={onOpenWaitlist}
              aria-expanded={waitlistOpen}
              aria-controls="waitlist-form"
            >
              Notify me on managed launch
            </button>
            <a className="btn btn-text" href="https://github.com/AppRafter/apprafter" target="_blank" rel="noreferrer" aria-label="View on GitHub">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <path d="M12 .5A12 12 0 0 0 0 12.5a12 12 0 0 0 8.2 11.4c.6.1.8-.3.8-.6v-2.1c-3.3.7-4-1.6-4-1.6-.6-1.4-1.4-1.8-1.4-1.8-1.1-.7.1-.7.1-.7 1.2.1 1.9 1.3 1.9 1.3 1.1 1.9 2.9 1.4 3.7 1 .1-.8.4-1.4.8-1.7-2.6-.3-5.4-1.3-5.4-6 0-1.3.5-2.4 1.2-3.2-.1-.3-.5-1.6.1-3.3 0 0 1-.3 3.3 1.2a11.4 11.4 0 0 1 6 0c2.3-1.5 3.3-1.2 3.3-1.2.6 1.7.2 3 .1 3.3.8.8 1.2 1.9 1.2 3.2 0 4.7-2.8 5.7-5.5 6 .4.4.8 1.1.8 2.3v3.3c0 .3.2.7.8.6A12 12 0 0 0 24 12.5 12 12 0 0 0 12 .5z" />
              </svg>
              View on GitHub
            </a>
          </div>
          <div className="status-badge">
            <span className="dot" />
            <span>{version} · MVP shipped on Tier 1 and Tier 2 · managed in development</span>
          </div>
          {waitlistOpen && <WaitlistForm />}
        </div>
        <HeroSnippet />
      </div>
    </section>
  );
};

// ============================================================
// Waitlist form
// ============================================================
const WaitlistForm = () => {
  const [email, setEmail] = React.useState("");
  const [useCase, setUseCase] = React.useState("");
  const [wantsCall, setWantsCall] = React.useState(false);
  const [submitted, setSubmitted] = React.useState(false);

  const submit = (e) => {
    e.preventDefault();
    if (!email || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) return;
    setSubmitted(true);
  };

  if (submitted) {
    return (
      <div className="waitlist" id="waitlist-form" role="status">
        <div className="waitlist-success">
          → We'll be in touch.{wantsCall ? " You'll get a separate email with a calendar link." : ""}
        </div>
      </div>
    );
  }

  return (
    <form className="waitlist" id="waitlist-form" onSubmit={submit} noValidate>
      <p className="copy">
        The managed version of AppRafter is in development. Drop your email — we'll let you know when it's ready. One email, no newsletter, no marketing drip.
      </p>
      <div className="field">
        <label htmlFor="wl-email">Email</label>
        <input
          id="wl-email"
          type="email"
          required
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="you@company.com"
          autoComplete="email"
        />
      </div>
      <div className="field">
        <label htmlFor="wl-use">What's your use case? <span className="faint">(optional)</span></label>
        <input
          id="wl-use"
          type="text"
          value={useCase}
          onChange={(e) => setUseCase(e.target.value)}
          placeholder="Small SaaS, 3 services, currently on Render"
        />
      </div>
      <label className="checkbox-row">
        <input
          type="checkbox"
          checked={wantsCall}
          onChange={(e) => setWantsCall(e.target.checked)}
        />
        <span>I'd like a short call to discuss my use case.</span>
      </label>
      <div className="waitlist-actions">
        <button type="submit" className="btn btn-primary">Notify me</button>
        <span className="faint" style={{ fontSize: 12 }}>Stored only for launch announcement.</span>
      </div>
    </form>
  );
};

// ============================================================
// Value props
// ============================================================
const ValueProps = () => {
  const props = [
    {
      title: "Deploy with a single manifest.",
      body: (
        <>
          Describe your application in CUE: <code className="mono">kind: Application</code>, declare dependencies through <code className="mono">needs.pg</code> / <code className="mono">needs.jetstream</code> / <code className="mono">needs.redis</code>. Deploy via CLI or through an AI agent over MCP — destructive operations gated by a CRD, no <code className="mono">kubectl delete</code> surprises. The platform handles the rest — Postgres clusters with backups, NATS streams with retention, Redis instances. No 400-line <code className="mono">values.yaml</code>. No drift between dev and prod.
        </>
      ),
      icon: (
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true">
          <path d="M4 4h16v16H4z" />
          <path d="M4 9h16" />
          <path d="M9 4v16" />
        </svg>
      ),
    },
    {
      title: "From one VDS to production, no scaling ceiling.",
      body: (
        <>
          The same manifest runs on a €5 Hetzner VDS (single node, Tier 1) and scales horizontally to an HA cluster of any size and node mix (Tier 2). When you grow, you add nodes — you don't migrate to a different platform when you hit your provider's vertical ceiling. No rewriting applications between dev and prod.
        </>
      ),
      icon: (
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true">
          <path d="M3 21h4v-6H3z" />
          <path d="M10 21h4v-11h-4z" />
          <path d="M17 21h4V4h-4z" />
        </svg>
      ),
    },
    {
      title: "Open source, no vendor lock-in.",
      body: (
        <>
          FSL-1.1-Apache-2.0, auto-converts to Apache 2.0 in two years. Everything runs on your hardware or in your cloud. The managed version is on the way for those who'd rather not run ops themselves. And when you want back to self-host from it — there'll be a CLI command for that. Not a philosophy, an architecture.
        </>
      ),
      icon: (
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true">
          <rect x="3" y="11" width="18" height="10" />
          <path d="M7 11V7a5 5 0 0 1 10 0v4" />
        </svg>
      ),
    },
  ];

  return (
    <section aria-label="Value props" data-screen-label="value-props">
      <div className="container">
        <div className="section-head">
          <div className="eyebrow">/ Why AppRafter</div>
          <h2>Three things you don't have to fight anymore.</h2>
        </div>
        <div className="value-grid">
          {props.map((p, i) => (
            <div className="value-card" key={i}>
              <div className="icon">{p.icon}</div>
              <div className="index">0{i + 1} / 03</div>
              <h3>{p.title}</h3>
              <p>{p.body}</p>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
};

// ============================================================
// Tier ladder
// ============================================================
const TierLadder = () => {
  const tiers = [
    {
      num: "Tier 1",
      title: "Single VDS",
      price: "From €5/mo",
      desc: "A single VDS with sane simplifications (no HA quorum, single-node database, no multi-tenancy). For side-projects and solo founders. Hetzner Cloud at launch.",
      status: "live",
      statusText: "Available now",
    },
    {
      num: "Tier 2",
      title: "Production cluster",
      price: "3+ nodes",
      desc: "Production. 3 or more nodes of any size and any count — grows with your project. Hetzner Cloud at launch.",
      status: "live",
      statusText: "Available now",
    },
    {
      num: "Tier 3",
      title: "Bare metal",
      price: "Dedicated EPYC",
      desc: "Bare metal. Dedicated EPYC servers. For when you need a performance ceiling above VPS.",
      status: "roadmap",
      statusText: "Roadmap · Phase 5+",
    },
    {
      num: "Tier 4",
      title: "Hyperscalers",
      price: "AWS · GCP · Azure",
      desc: "Hyperscalers. Primarily for cases where regulation requires these specific providers.",
      status: "roadmap",
      statusText: "Roadmap · Phase 6+",
    },
  ];

  return (
    <section aria-label="Tier ladder" id="tiers" data-screen-label="tiers">
      <div className="container">
        <div className="section-head">
          <div className="eyebrow">/ Tier ladder</div>
          <h2>One Application manifest. The backing changes — not your app.</h2>
        </div>
        <div className="ladder-wrap">
          <div className="stair" aria-hidden="true">
            {[1,2,3,4].map((n, i) => (
              <div
                key={i}
                className={"step" + (n <= 2 ? " live" : "")}
                style={{ height: 20 + n * 16 }}
              >
                <span className="label">T{n}</span>
              </div>
            ))}
          </div>
          <div className="tier-cards">
            {tiers.map((t, i) => (
              <div className="tier-card" key={i}>
                <span className="tier-num">{t.num}</span>
                <h3>{t.title}</h3>
                <div className="price">{t.price}</div>
                <p>{t.desc}</p>
                <span className={"status " + t.status}>{t.statusText}</span>
              </div>
            ))}
          </div>
          <div className="orthogonal-note">
            <strong>Confidential containers</strong> — orthogonal capability, not a tier. Available on any hardware that supports TDX or SEV-SNP. Shipping Phase 6+.
          </div>
        </div>
      </div>
    </section>
  );
};

// ============================================================
// Comparison table
// ============================================================
const Comparison = () => {
  const rows = [
    {
      label: "Price",
      self: <span className="price-cell">Free<sup>†</sup><span className="price-sub">FSL-1.1-Apache-2.0</span></span>,
      managed: <span className="price-cell">€10/mo per cluster<span className="price-sub">Hosted Services (launch)<br />+ €10/mo Operations add-on (Phase 4.5+)</span></span>,
      turnkey: <span className="price-cell">From €30/mo (Tier 1)<span className="price-sub">Server cost + reseller markup<br />+ €20/mo per cluster</span></span>,
    },
    {
      label: "Who runs the infra",
      self: "You",
      managed: "You (your Hetzner account)",
      turnkey: "We do",
    },
    {
      label: "Who runs the UI",
      self: "You (optional)",
      managed: "We do",
      turnkey: "We do",
    },
    {
      label: "Where your data lives",
      self: "With you",
      managed: "With you — your cluster, your control plane, your databases",
      turnkey: "With us, in our Hetzner account",
    },
    {
      label: "What we have visibility into",
      self: "Nothing",
      managed: "Only metadata — architectural guarantee from cluster ownership",
      turnkey: "Metadata by design (Minimal Data Exposure architecture), but the account is ours — so the guarantee is policy-level rather than structural",
    },
    {
      label: "Exit when you stop paying",
      self: <span className="faint">n/a</span>,
      managed: "Your cluster keeps running. OSS takes over management. UI/Backstage you can self-host (we ship the tooling). You only lose the thin cloud-native premium layer — AI insights, cross-cluster aggregator, smart bill analysis.",
      turnkey: "Cross-cluster MigrationPlan tooling (Phase 8+) for orchestrated migration.",
    },
  ];

  return (
    <section aria-label="Self-host vs Managed vs Turnkey" id="offerings" data-screen-label="offerings">
      <div className="container">
        <div className="section-head">
          <div className="eyebrow">/ How to run AppRafter</div>
          <h2>Three ways to run AppRafter.</h2>
        </div>
        <table className="compare-table">
          <thead>
            <tr>
              <th scope="col"></th>
              <th scope="col">Self-host</th>
              <th scope="col" className="featured">Managed <span className="mono faint" style={{ fontSize: 11, marginLeft: 6 }}>(waitlist)</span></th>
              <th scope="col">Turnkey <span className="mono faint" style={{ fontSize: 11, marginLeft: 6 }}>(roadmap)</span></th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={i}>
                <td>{r.label}</td>
                <td>{r.self}</td>
                <td>{r.managed}</td>
                <td>{r.turnkey}</td>
              </tr>
            ))}
            <tr className="status-row">
              <td>Status</td>
              <td><span className="status-pill live">Available now</span></td>
              <td><span className="status-pill is-waitlist">Waitlist</span></td>
              <td><span className="status-pill is-roadmap">Roadmap</span></td>
            </tr>
          </tbody>
        </table>
        <p className="footnote">
          <sup>†</sup> FSL-1.1-Apache-2.0 allows any use — personal, internal business, commercial workloads — except offering AppRafter itself as a managed service to third parties. After two years, the license auto-converts to plain Apache 2.0 and that restriction lifts.
        </p>

        <div className="transparency-grid">
          <div className="transparency-card">
            <h4><span className="kicker">4.5.1 / Pricing</span>Everything you pay for is in this table.</h4>
            <p>No hidden tiers, no per-seat upsells, no "free tier that ends once you outgrow it". Prepaid model — what you see is what you pay, no metered surprises. Annual billing optional, with a <strong>save two months</strong> discount.</p>
            <p>14-day trial without a card, once per account.</p>
          </div>
          <div className="transparency-card">
            <h4><span className="kicker">4.5.2 / Anti-lock</span>Architectural, not promised.</h4>
            <p>In <strong>Managed</strong>, your cluster physically lives on your infrastructure. Cancel — the cluster keeps running, you lose the premium layer (which you can mostly self-host; we ship the tooling). An <strong>architectural fact</strong>, not a service-level promise.</p>
            <p>For <strong>Turnkey</strong>, we invest engineering cycles into your <strong>exit path</strong>. Cross-cluster MigrationPlan is <strong>planned as shipped tooling</strong> (Phase 8+) — <code>apprafter migration plan</code> — for moving to self-host or another provider.</p>
          </div>
          <div className="transparency-card">
            <h4><span className="kicker">4.5.3 / Alignment</span>Our model grows with you.</h4>
            <p>Per-cluster billing means our revenue grows when your deployments grow. We don't make more by locking you in or up-selling seats. We make more when you scale — more clusters, more workloads, more compute.</p>
            <p>When you stay small, we stay small with you. An <strong>incentive structure</strong>, encoded in how we charge.</p>
          </div>
        </div>
      </div>
    </section>
  );
};

// ============================================================
// Boring tech
// ============================================================
const BoringTech = () => {
  const underHood = [
    ["Talos Linux", "Immutable OS, API-driven. Fewer snowflakes than a regular Linux node."],
    ["k3s / Cilium", "k8s core + eBPF networking. Network policies, observability via Hubble, egress gateway."],
    ["NATS JetStream", "Event/messaging backbone and control plane storage (via the community kine-nats adapter)."],
    ["CloudNativePG", "Postgres operator with replication, backups, point-in-time recovery."],
    ["Dragonfly", "Redis-compatible, scales better on a single node."],
    ["ClickHouse", "Logs, traces, application analytics."],
    ["OpenBao", "Secrets management (HashiCorp Vault fork, BSL-free)."],
    ["Backstage", "Developer portal with TypeScript plugins."],
    ["Kamaji + Capsule", "Hard multi-tenancy (Phase 5+)."],
    ["cert-manager · external-dns · KEDA", "Standard k8s add-ons."],
  ];
  const ourCode = [
    ["Rust operator on kube-rs", "Reconciles Application, ResourceClaim, ServiceProvider, MigrationPlan, Tenant, AccessGrant."],
    ["CUE-based admission webhook", "Type-safe validation with line-level errors before apply."],
    ["apprafter CLI", "Bootstrap, manifest workflow, dev mode, migration tooling."],
    ["MCP server with agentic safety gate", "Hosted MCP endpoint proxying through customer agent. Operations classified by risk taxonomy — safe / reversible / bounded write / destructive — with automatic MigrationPlan creation for destructive operations."],
    ["NATS-based audit log layer on top of kine", "Turning the control plane's NATS backing store into a replayable platform event log (Tier 2+; opt-in upgrade on Tier 1)."],
    ["MigrationPlan reconciler", "Destructive-change gating with explicit approval."],
    ["ResourceClaim / ServiceProvider primitives", "Typed contract for platform services."],
  ];
  return (
    <section aria-label="Boring tech" id="stack" data-screen-label="stack">
      <div className="container">
        <div className="section-head">
          <div className="eyebrow">/ The stack</div>
          <h2>Boring tech, opinionated glue.</h2>
          <p className="lede">
            We don't reinvent the Kubernetes control plane. Under the hood — well-known, proven components. The real work is opinionated composition and a thin layer of code where no ready solution exists.
          </p>
        </div>
        <div className="boring-grid">
          <div className="boring-col">
            <h3>Under the hood</h3>
            <ul className="tech-list">
              {underHood.map(([name, desc], i) => (
                <li key={i}>
                  <span className="name">{name}</span>
                  <span className="desc">{desc}</span>
                </li>
              ))}
            </ul>
          </div>
          <div className="boring-col">
            <h3>What we wrote ourselves</h3>
            <ul className="tech-list">
              {ourCode.map(([name, desc], i) => (
                <li key={i}>
                  <span className="name"><span className="ours">{name}</span></span>
                  <span className="desc">{desc}</span>
                </li>
              ))}
            </ul>
          </div>
        </div>
        <p className="closing">
          This is a thin layer <strong>on top of</strong> proven components — only where no ready solution exists. Boring, and that's intentional. <strong>Boring tech</strong> is easier to debug, easier to hire for, easier to keep running.
        </p>
      </div>
    </section>
  );
};

// ============================================================
// Scaling journey — central thesis visual
// ============================================================
const ScalingJourney = () => (
  <section aria-label="Scaling journey" id="scaling-journey" data-screen-label="scaling-journey">
    <div className="container">
      <div className="section-head">
        <div className="eyebrow">/ How scaling works</div>
        <h2>One file. From single node to HA cluster.</h2>
        <p className="lede">
          Most PaaS platforms cap at vertical scaling — when you grow, you migrate. Most Kubernetes distros assume you're starting big. AppRafter is the same platform from a €5 VPS to a multi-node HA cluster, and the same <code className="mono">Application.cue</code> deploys to both.
        </p>
      </div>

      <div className="journey">
        {/* Left: Tier 1 — single node */}
        <div className="journey-side">
          <div className="journey-eyebrow">Tier 1 · €5 VPS</div>
          <div className="journey-stage">
            <div className="journey-node solo">
              <span className="node-label">node-1</span>
            </div>
            <div className="journey-file">
              <span className="file-name">Application.cue</span>
              <span className="file-meta">23 LOC</span>
            </div>
          </div>
          <div className="journey-caption">A single VDS. Single-node Postgres. SealedSecrets. Sane defaults.</div>
        </div>

        {/* Middle: arrow */}
        <div className="journey-arrow">
          <div className="arrow-bar">
            <span className="arrow-label">Tier upgrade</span>
          </div>
          <svg viewBox="0 0 80 28" aria-hidden="true" className="arrow-head">
            <path d="M0 14 H72 M60 4 L72 14 L60 24" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="square" />
          </svg>
        </div>

        {/* Right: Tier 2 — HA cluster */}
        <div className="journey-side">
          <div className="journey-eyebrow">Tier 2 · HA cluster</div>
          <div className="journey-stage">
            <div className="journey-cluster">
              <div className="journey-node"><span className="node-label">cp-1</span></div>
              <div className="journey-node"><span className="node-label">cp-2</span></div>
              <div className="journey-node"><span className="node-label">cp-3</span></div>
              <div className="journey-node small"><span className="node-label">w-1</span></div>
              <div className="journey-node small"><span className="node-label">w-2</span></div>
              <div className="journey-node small"><span className="node-label">w-N</span></div>
            </div>
            <div className="journey-file">
              <span className="file-name">Application.cue</span>
              <span className="file-meta">identical · 23 LOC</span>
            </div>
          </div>
          <div className="journey-caption">3+ nodes. CloudNativePG. Replayable audit log. OpenBao for secrets.</div>
        </div>
      </div>

      <div className="journey-footer">
        <span className="footer-kicker">No rewrite.</span>
        <span className="footer-kicker">No migration tool.</span>
        <span className="footer-kicker">One CUE manifest.</span>
      </div>

      <div className="journey-caveat">
        <strong>Today:</strong> Tier 1 and Tier 2 ready at signup. T1→T2 migration tool ships in the first post-launch bundle (~2–4 weeks after release).
      </div>
    </div>
  </section>
);

// ============================================================
// Structural advantages
// ============================================================
const Advantages = () => {
  const blocks = [
    {
      kfm: "KFM #1",
      title: "Manifest portability across tiers.",
      lead: (
        <><strong>The same Application manifest is designed to run from local dev (k3d) through a single VDS, multi-node clusters, bare metal, and hyperscalers — without rewrites.</strong> Not "we tested in a few environments" — a structural property: one CUE schema, one operator, one ResourceClaim contract. Only the backing changes.</>
      ),
      detail: "Today it works on local dev, Tier 1 (single VDS), and Tier 2 (multi-node). Tier 3 is in Phase 5+, Tier 4 in Phase 6+.",
      phase: "Today: Tier 1 + Tier 2",
    },
    {
      kfm: "KFM #21",
      title: "Replayable audit log out of the box.",
      lead: (
        <><strong>The control plane runs on kine + NATS JetStream, which means every platform operation can be replayed from the event log.</strong> Not a separate audit pipeline, not an add-on to etcd. Compliance-friendly <strong>architecturally</strong>, not by promise. A competitor would have to swap out their control plane storage layer to match — a major architectural commitment.</>
      ),
      detail: "Works today on Tier 2 and above. On Tier 1 the control plane uses kine + SQLite for simplicity; replayable log is available as an opt-in upgrade.",
      phase: "Today: Tier 2+",
    },
    {
      kfm: "KFM #2",
      title: "Managed → self-host: trivial exit.",
      lead: (
        <><strong>An architectural consequence of how we run Managed: only the UI and operations layer is hosted by us. Your cluster always stays with you.</strong> Other managed platforms keep workloads on their infrastructure — replicating our model would conflict with their incentive structure. Workloads-live-with-us is how the dominant model monetizes; we monetize differently.</>
      ),
      detail: "Full CLI in Phase 4. Principle in place from Phase 1.",
      phase: "Principle: Phase 1 · Full CLI: Phase 4",
    },
    {
      kfm: "KFM #9",
      title: "Six platform services through one typed primitive.",
      lead: (
        <><strong>Not a Helm wrapper over Postgres.</strong> A standardized CRD contract via ResourceClaim and ServiceProvider, which means a drop-in replacement (Postgres → AlloyDB, Redis → Valkey, S3 → Garage) is a container-level swap of the ServiceProvider — not an application rewrite. Competitors who expose services through Helm values would have to change their resource model from the ground up.</>
      ),
      detail: "Works today: Postgres, JetStream, Redis (Phase 2). Coming: ClickHouse, S3, Notifications (Phase 3).",
      phase: "Today: Postgres · JetStream · Redis",
    },
    {
      kfm: "KFM #3",
      title: "Agentic operations with structural safety.",
      lead: (
        <><strong>An AI agent can deploy, scale, restart, or migrate your apps through the MCP server — every destructive operation is gated by a MigrationPlan CRD with explicit human approval.</strong> Not a soft API check that an agent can bypass. The classifier runs in the operator: <code className="mono">delete_app prod</code> creates a MigrationPlan in your cluster, pauses, waits for <code className="mono">apprafter migration approve</code>. A competitor without a CRD-based primitive would have to bolt safety on as a side-channel, which agents can route around.</>
      ),
      detail: "Works today via CLI approvals. Backstage MigrationPlan plugin coming Phase 4+ (post-launch first bundle).",
      phase: "Today: CLI approval flow",
      featured: true,
    },
  ];
  return (
    <section aria-label="Structural advantages" id="architecture" data-screen-label="architecture">
      <div className="container">
        <div className="section-head">
          <div className="eyebrow">/ Structural advantages</div>
          <h2>What the architecture gives you.</h2>
          <p className="lede">Each claim is supported by the shape of the system, not by maintenance promises.</p>
        </div>
        <div className="advantages">
          {blocks.map((b, i) => (
            <div className={"advantage" + (b.featured ? " advantage-featured" : "")} key={i}>
              <h3>{b.title}</h3>
              <p>{b.lead}</p>
              <p className="faint" style={{ fontStyle: "italic", fontSize: 14 }}>{b.detail}</p>
              <span className="phase-tag">{b.phase}</span>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
};

// ============================================================
// Roadmap
// ============================================================
const Roadmap = () => {
  const phases = [
    {
      num: "Phase 4",
      title: "Managed offering launch",
      items: [
        "Export-to-self-host CLI (fully functional)",
        "MCP-native managed (full integration)",
        "Minimal Data Exposure ADR + audit",
        "Hosted Services + Managed Operations tiers shipped",
      ],
    },
    {
      num: "Phase 5+",
      title: "Production Tier 3 + multi-tenancy",
      items: [
        "Tier 3 (Talos + LINSTOR on bare metal)",
        "Hard multi-tenancy via Kamaji + Capsule",
        "Turnkey Cloud launches (Tier 1–3)",
        "Live platform demo via self-hosted AccessGrant",
        "One-time migration toolkit (Product 1: cloud-foreign → AppRafter)",
      ],
    },
    {
      num: "Phase 6+",
      title: "Confidential workloads",
      items: [
        "Tier 4 confidential containers (Kata-CC) in opinionated wrapper",
        "Full T1 → T4 manifest portability complete",
      ],
    },
    {
      num: "Phase 8+",
      title: "Cross-cluster federation",
      items: [
        "Cross-cluster MigrationPlan (Product 2): sub-second cutover between clusters",
        "DR failover as an orchestrated operation",
        "Region migration within Turnkey — invisible to the customer",
      ],
    },
  ];
  return (
    <section aria-label="Roadmap" id="roadmap" data-screen-label="roadmap">
      <div className="container">
        <div className="section-head">
          <div className="eyebrow">/ Roadmap</div>
          <h2>Roadmap.</h2>
          <p className="lede">Phases, not quarters. Each one a finished product on its own, not "an MVP we'll polish later".</p>
        </div>
        <div className="roadmap">
          {phases.map((p, i) => (
            <div className="roadmap-phase" key={i} id={`roadmap-phase-${p.num.toLowerCase().replace(/\W+/g, "-")}`}>
              <div className="phase-meta">
                <div className="phase-num">{p.num}</div>
                <h3>{p.title}</h3>
              </div>
              <ul>
                {p.items.map((item, j) => <li key={j}>{item}</li>)}
              </ul>
            </div>
          ))}
        </div>
        <p className="roadmap-closing">
          Roadmap is driven by shipped features, not PR dates. Each phase is a finished product on its own, not "an MVP we'll polish later".
        </p>
      </div>
    </section>
  );
};

// ============================================================
// Bootstrap strip + Footer
// ============================================================
const BootstrapStrip = () => (
  <div className="container">
    <div className="bootstrap-strip">
      AppRafter is a bootstrap project. No VC funding, no exit pressure — we grow with our customers, not at their expense.
    </div>
  </div>
);

const Footer = ({ logoVariant }) => (
  <footer className="site-footer">
    <div className="container">
      <div className="footer-grid">
        <div className="footer-brand">
          <Brand variant={logoVariant} size={22} />
          <p className="desc">
            An opinionated PaaS on Kubernetes. Open source, bootstrap-funded, scaling with you instead of around you.
          </p>
        </div>
        <div>
          <h4>Project</h4>
          <ul>
            <li><a href="https://github.com/AppRafter/apprafter/blob/main/spec.md" target="_blank" rel="noreferrer">Spec</a></li>
            <li><a href="https://github.com/AppRafter/apprafter" target="_blank" rel="noreferrer">GitHub</a></li>
            <li><a href="#roadmap">Roadmap</a></li>
            <li><a href="https://github.com/AppRafter/apprafter#readme" target="_blank" rel="noreferrer">Docs <span className="faint mono" style={{ fontSize: 10 }}>SOON</span></a></li>
          </ul>
        </div>
        <div>
          <h4>Legal</h4>
          <ul>
            <li><a href="https://github.com/AppRafter/apprafter/blob/main/LICENSE" target="_blank" rel="noreferrer">License — FSL-1.1-Apache-2.0</a></li>
            <li><a href="#privacy">Privacy</a></li>
            <li><a href="#terms">Terms</a></li>
          </ul>
        </div>
        <div>
          <h4>Author</h4>
          <ul>
            <li><a href="#author">Founder's site</a></li>
            <li><a href="https://github.com/AppRafter" target="_blank" rel="noreferrer">GitHub org</a></li>
          </ul>
        </div>
      </div>
      <div className="footer-bottom">
        <span>© 2026 AppRafter · <a href="https://apprafter.dev" className="muted">apprafter.dev</a></span>
        <span className="mono" style={{ fontSize: 12 }}>FSL-1.1-Apache-2.0 · auto-converts to Apache 2.0 after 2 years</span>
        <span>Bootstrap-funded. Built solo.</span>
      </div>
    </div>
  </footer>
);

Object.assign(window, {
  Header, Hero, ValueProps, ScalingJourney, TierLadder, Comparison,
  BoringTech, Advantages, Roadmap, BootstrapStrip, Footer,
  WaitlistForm,
});
