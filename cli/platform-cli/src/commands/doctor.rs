// SPDX-License-Identifier: FSL-1.1-MIT
//! `apprafter doctor` — self-diagnostic. Walks the active
//! target's stored state + reachability + the surrounding shell
//! environment and prints a PASS / WARN / FAIL line per check.
//!
//! Per `cli-dx-task.md` §5.9 the command serves three audiences:
//! - new users troubleshooting "why doesn't this work",
//! - CI smoke gates wiring `apprafter doctor` in as a quality
//!   precondition for downstream stages,
//! - bug-report templates (the output is structured enough that
//!   pasting it into an issue is genuinely useful).
//!
//! Exit code policy:
//! - 0  → no FAIL checks (WARNs allowed).
//! - 1  → at least one FAIL.
//!
//! WARNs are informational (missing optional dep, unverified
//! token because `--no-ping` was passed). FAILs are real broken
//! state (token rejected, can't reach API, ssh-key path
//! configured but file missing).

use std::process::{Command, Output};
use std::time::Instant;

use cli_core::target::{
    default_config_root, load_global_config, load_target, validate_hetzner_token_format, Target,
    TargetStorePaths,
};
use cli_core::{CliError, Result};
use cli_providers::{HetznerCloudValidator, ProviderValidator};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

/// Per-check verdict. PASS / WARN / FAIL are ordered by
/// severity; the `Display` impl renders the matching glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    fn glyph(&self) -> &'static str {
        match self {
            Self::Pass => "✓",
            Self::Warn => "⚠",
            Self::Fail => "✗",
        }
    }
}

/// One row of the doctor report. `detail` lands on the same line
/// as the check name (in parens); `hint` lands on its own
/// indented line below — used to direct the operator at the
/// specific next action ("run `apprafter target add <name>
/// --renew`").
#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: Option<String>,
    pub hint: Option<String>,
}

impl Check {
    pub fn pass(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Pass,
            detail: None,
            hint: None,
        }
    }

    pub fn warn(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warn,
            detail: None,
            hint: None,
        }
    }

    pub fn fail(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            detail: None,
            hint: None,
        }
    }

    pub fn with_detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }

    pub fn with_hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
}

/// Whole-run report — target half + environment half — kept as
/// data so we can unit-test the rendering separately from the
/// side-effecting checks themselves.
#[derive(Debug, Default)]
pub struct DoctorReport {
    /// Name of the target we inspected, when one was resolved.
    pub target_name: Option<String>,
    pub target_checks: Vec<Check>,
    pub env_checks: Vec<Check>,
}

impl DoctorReport {
    pub fn iter_checks(&self) -> impl Iterator<Item = &Check> {
        self.target_checks.iter().chain(self.env_checks.iter())
    }

    pub fn passed(&self) -> usize {
        self.iter_checks()
            .filter(|c| c.status == CheckStatus::Pass)
            .count()
    }

    pub fn warned(&self) -> usize {
        self.iter_checks()
            .filter(|c| c.status == CheckStatus::Warn)
            .count()
    }

    pub fn failed(&self) -> usize {
        self.iter_checks()
            .filter(|c| c.status == CheckStatus::Fail)
            .count()
    }

    pub fn has_failures(&self) -> bool {
        self.failed() > 0
    }
}

// ---------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------

pub fn run(target_override: Option<&str>, no_ping: bool) -> Result<()> {
    info!(target_override, no_ping, "doctor invoked");

    let paths = TargetStorePaths::for_root(default_config_root()?);
    let mut report = DoctorReport::default();

    // Target half. Either inspect the supplied --target, the
    // active target if any, or surface an onboarding error.
    let resolved = match target_override {
        Some(n) => Some(n.to_string()),
        None => load_global_config(&paths)?
            .map(|g| g.active_target)
            .filter(|s| !s.is_empty()),
    };

    match resolved {
        Some(name) => {
            report.target_name = Some(name.clone());
            report.target_checks = build_target_checks(&paths, &name, no_ping);
        }
        None => {
            return Err(CliError::Other(
                "no active target and no `--target` supplied. Run `apprafter target add` to configure one, then re-run `apprafter doctor`."
                    .to_string(),
            ));
        }
    }

    // Environment half — runs regardless of target state.
    report.env_checks = build_env_checks();

    print_report(&report);

    if report.has_failures() {
        std::process::exit(1);
    }
    Ok(())
}

// ---------------------------------------------------------------
// Target-side checks
// ---------------------------------------------------------------

fn build_target_checks(paths: &TargetStorePaths, name: &str, no_ping: bool) -> Vec<Check> {
    let mut out = Vec::new();

    // Config file readable. `load_target` already does this — we
    // call it once and reuse the result across the rest of the
    // target checks.
    let target = match load_target(paths, name) {
        Ok(t) => {
            out.push(
                Check::pass("Config file readable")
                    .with_detail(paths.target_config_file(name).display().to_string()),
            );
            t
        }
        Err(CliError::TargetNotFound { available, .. }) => {
            out.push(
                Check::fail(format!("Target `{name}` exists")).with_hint(format!(
                    "available targets: {available}. Run `apprafter target add {name}` to create."
                )),
            );
            return out;
        }
        Err(e) => {
            out.push(
                Check::fail("Config file readable")
                    .with_detail(paths.target_config_file(name).display().to_string())
                    .with_hint(format!("{e}")),
            );
            return out;
        }
    };

    out.push(check_credentials_file(paths, name));
    out.push(check_provider_known(&target));
    out.push(check_token_format(&target));
    out.push(check_token_ping(&target, no_ping));
    out.push(check_ssh_key(&target));

    out
}

fn check_credentials_file(paths: &TargetStorePaths, name: &str) -> Check {
    let path = paths.target_credentials_file(name);
    if !path.exists() {
        return Check::fail("Credentials file present")
            .with_detail(path.display().to_string())
            .with_hint(format!(
                "run `apprafter target add {name} --renew --token <X>` to create credentials.yaml"
            ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                return Check::fail("Credentials file present")
                    .with_detail(path.display().to_string())
                    .with_hint(format!("cannot stat file: {e}"));
            }
        };
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Check::warn(format!("Credentials file mode 0600 (got {mode:o})"))
                .with_detail(path.display().to_string())
                .with_hint(format!(
                    "fix with `chmod 600 {}` so other local users can't read your token",
                    path.display()
                ));
        }
        Check::pass("Credentials file present (mode 0600)").with_detail(path.display().to_string())
    }
    #[cfg(not(unix))]
    {
        Check::pass("Credentials file present").with_detail(path.display().to_string())
    }
}

fn check_provider_known(target: &Target) -> Check {
    const SUPPORTED: &[&str] = &["hetzner-cloud"];
    let p = &target.config.provider;
    if SUPPORTED.contains(&p.as_str()) {
        Check::pass(format!("Provider `{p}` supported"))
    } else {
        Check::fail(format!("Provider `{p}` supported")).with_hint(format!(
            "supported in this build: {}. Future plugins may add more.",
            SUPPORTED.join(", ")
        ))
    }
}

fn check_token_format(target: &Target) -> Check {
    let Some(tok) = target.credentials.hetzner_token.as_deref() else {
        return Check::fail("Hetzner token present").with_hint(format!(
            "run `apprafter target add {name} --renew --token <X>` to add credentials",
            name = target.name
        ));
    };
    match validate_hetzner_token_format(tok) {
        Ok(()) => Check::pass("Token format valid")
            .with_detail(format!("{} chars, alphanumeric", tok.len())),
        Err(reason) => Check::fail("Token format valid")
            .with_detail(reason)
            .with_hint(format!(
                "run `apprafter target add {name} --renew --token <X>` with a fresh token",
                name = target.name
            )),
    }
}

fn check_token_ping(target: &Target, no_ping: bool) -> Check {
    if no_ping {
        return Check::warn("Token verified against provider API")
            .with_detail("skipped — `--no-ping`".to_string());
    }
    let Some(tok) = target.credentials.hetzner_token.as_deref() else {
        return Check::warn("Token verified against provider API")
            .with_detail("skipped — no token stored".to_string());
    };
    if target.config.provider != "hetzner-cloud" {
        return Check::warn("Token verified against provider API").with_detail(format!(
            "no validator wired for provider `{}`",
            target.config.provider
        ));
    }
    let validator = HetznerCloudValidator::new(hcloud_base_url(), tok);
    let start = Instant::now();
    match validator.validate_credentials() {
        Ok(()) => {
            let ms = start.elapsed().as_millis();
            Check::pass("Token verified against provider API")
                .with_detail(format!("Hetzner Cloud /v1/locations, {ms} ms"))
        }
        Err(CliError::Hetzner {
            status: 401,
            message,
            ..
        }) => Check::fail("Token verified against provider API")
            .with_detail(format!("HTTP 401: {message}"))
            .with_hint(format!(
                "token rejected; run `apprafter target add {name} --renew` with a fresh token from Hetzner Cloud Console → Security → API Tokens",
                name = target.name
            )),
        Err(CliError::Hetzner {
            status, message, ..
        }) => Check::fail("Token verified against provider API")
            .with_detail(format!("HTTP {status}: {message}"))
            .with_hint("provider API returned an unexpected error — retry, then check Hetzner status page"),
        Err(e) => Check::fail("Token verified against provider API")
            .with_detail(format!("{e}"))
            .with_hint(
                "could not reach the provider API — check DNS / network / proxy, or rerun with `--no-ping` to skip this check",
            ),
    }
}

fn check_ssh_key(target: &Target) -> Check {
    match target.config.ssh_key_path.as_ref() {
        None => Check::warn("SSH key path configured").with_hint(
            "no SSH key in target config — `apprafter init` / `apply` will refuse until you set one via `apprafter target add <name> --force --ssh-key <path>` or via the wizard",
        ),
        Some(p) => {
            if !p.exists() {
                return Check::fail("SSH key readable")
                    .with_detail(p.display().to_string())
                    .with_hint("file does not exist; the path stored in target config may be stale");
            }
            match std::fs::read_to_string(p) {
                Ok(body) => {
                    let algo = body
                        .split_whitespace()
                        .next()
                        .unwrap_or("(unknown)")
                        .to_string();
                    Check::pass("SSH key readable")
                        .with_detail(format!("{} ({})", p.display(), algo))
                }
                Err(e) => Check::fail("SSH key readable")
                    .with_detail(p.display().to_string())
                    .with_hint(format!("cannot read file: {e}")),
            }
        }
    }
}

// ---------------------------------------------------------------
// Environment-side checks
// ---------------------------------------------------------------

fn build_env_checks() -> Vec<Check> {
    vec![
        check_tool("kubectl", &["version", "--client"]),
        check_tool("helm", &["version", "--short"]),
        check_tool("ssh", &["-V"]),
        check_dns_resolves("api.hetzner.cloud"),
    ]
}

/// Probe for a binary on PATH. Missing = WARN (the platform's
/// operational commands still install / use the tool through
/// `cargo run`-style paths in development; doctor isn't the
/// authoritative installer). Found = PASS with the first
/// non-empty line of the tool's `--version`-style output as
/// the detail.
fn check_tool(name: &str, version_args: &[&str]) -> Check {
    let out = Command::new(name).args(version_args).output();
    match out {
        Ok(o) if o.status.success() => Check::pass(format!("`{name}` on PATH"))
            .with_detail(first_nonempty_line(&o).unwrap_or_else(|| "version unknown".to_string())),
        Ok(o) => {
            // Some tools (ssh -V) write to stderr and exit 0,
            // others exit non-zero on `--version`. Be lenient:
            // if we got *some* output, treat it as PASS.
            if let Some(line) = first_nonempty_line(&o) {
                Check::pass(format!("`{name}` on PATH")).with_detail(line)
            } else {
                Check::warn(format!("`{name}` on PATH")).with_hint(format!(
                    "{name} exited {:?} without recognisable version output",
                    o.status.code()
                ))
            }
        }
        Err(e) => Check::warn(format!("`{name}` on PATH")).with_hint(format!(
            "not found ({e}); install when you need it — `apprafter doctor` keeps running",
        )),
    }
}

fn first_nonempty_line(o: &Output) -> Option<String> {
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    [stdout, stderr]
        .iter()
        .flat_map(|s| s.lines())
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(String::from)
}

fn check_dns_resolves(host: &str) -> Check {
    use std::net::ToSocketAddrs;
    let target = format!("{host}:443");
    match target.to_socket_addrs() {
        Ok(iter) => {
            // `iter` is mutable inside the arm body; we just need
            // to know there's at least one address.
            let mut iter = iter;
            if iter.next().is_some() {
                Check::pass(format!("DNS resolves `{host}`")).with_detail("443/tcp")
            } else {
                Check::fail(format!("DNS resolves `{host}`"))
                    .with_hint("resolver returned an empty address list")
            }
        }
        Err(e) => Check::fail(format!("DNS resolves `{host}`")).with_hint(format!(
            "resolver error: {e}. Check /etc/resolv.conf or VPN."
        )),
    }
}

// ---------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------

pub fn print_report(report: &DoctorReport) {
    if let Some(name) = &report.target_name {
        println!("Checking target `{name}`...");
        for c in &report.target_checks {
            print_check_line(c);
        }
        println!();
    }
    println!("Checking environment...");
    for c in &report.env_checks {
        print_check_line(c);
    }
    println!();
    print_summary(report);
}

fn print_check_line(c: &Check) {
    match &c.detail {
        Some(d) => println!("  {} {} ({})", c.status.glyph(), c.name, d),
        None => println!("  {} {}", c.status.glyph(), c.name),
    }
    if let Some(h) = &c.hint {
        println!("      hint: {h}");
    }
}

fn print_summary(report: &DoctorReport) {
    let p = report.passed();
    let w = report.warned();
    let f = report.failed();
    let total = p + w + f;
    let target_blurb = match &report.target_name {
        Some(name) => format!(" for target `{name}`"),
        None => String::new(),
    };
    if f > 0 {
        println!(
            "{total} checks{target_blurb}: {p} passed, {w} warning(s), {f} FAIL — fix the FAILs and rerun `apprafter doctor`."
        );
    } else if w > 0 {
        println!(
            "{total} checks{target_blurb}: {p} passed, {w} warning(s). Ready to go; review warnings if they apply to your use case."
        );
    } else {
        println!(
            "{total} checks{target_blurb}: {p} passed. All good — ready for `apprafter init` / `apprafter apply`."
        );
    }
}

// ---------------------------------------------------------------
// Tests for pure helpers + rendering. The full `run` orchestrator
// is exercised by integration tests in tests/doctor_test.rs.
// ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_status_glyph_renders_distinctly() {
        assert_eq!(CheckStatus::Pass.glyph(), "✓");
        assert_eq!(CheckStatus::Warn.glyph(), "⚠");
        assert_eq!(CheckStatus::Fail.glyph(), "✗");
    }

    #[test]
    fn report_counters_split_by_status() {
        let report = DoctorReport {
            target_name: Some("t".into()),
            target_checks: vec![
                Check::pass("a"),
                Check::pass("b"),
                Check::warn("c"),
                Check::fail("d"),
            ],
            env_checks: vec![Check::pass("e"), Check::warn("f")],
        };
        assert_eq!(report.passed(), 3);
        assert_eq!(report.warned(), 2);
        assert_eq!(report.failed(), 1);
        assert!(report.has_failures());
    }

    #[test]
    fn report_has_failures_returns_false_when_only_warns() {
        let report = DoctorReport {
            target_name: None,
            target_checks: vec![],
            env_checks: vec![Check::warn("only-warn")],
        };
        assert!(!report.has_failures());
    }

    #[test]
    fn check_dns_resolves_localhost_passes() {
        // 127.0.0.1 always resolves locally even without DNS;
        // good cheap sanity check that the function returns the
        // PASS branch on a known-good host.
        let c = check_dns_resolves("127.0.0.1");
        assert_eq!(c.status, CheckStatus::Pass);
    }

    #[test]
    fn check_dns_resolves_invalid_tld_fails() {
        // RFC 6761 `.invalid` is reserved as never-resolvable.
        let c = check_dns_resolves("doctor-probe-host.invalid");
        assert_eq!(c.status, CheckStatus::Fail);
    }

    #[test]
    fn check_tool_warns_on_missing_binary() {
        // No system ships a binary called this; the function
        // must downgrade to WARN with a hint.
        let c = check_tool("apprafter-doctor-no-such-binary", &["--version"]);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.hint.is_some());
    }

    #[test]
    fn check_provider_known_fails_for_unknown_provider() {
        let t = make_target("aws-bedrock", None);
        let c = check_provider_known(&t);
        assert_eq!(c.status, CheckStatus::Fail);
    }

    #[test]
    fn check_provider_known_passes_for_hetzner() {
        let t = make_target("hetzner-cloud", None);
        let c = check_provider_known(&t);
        assert_eq!(c.status, CheckStatus::Pass);
    }

    #[test]
    fn check_token_format_passes_canonical_token() {
        let token = "a".repeat(64);
        let t = make_target("hetzner-cloud", Some(token));
        let c = check_token_format(&t);
        assert_eq!(c.status, CheckStatus::Pass);
        assert!(c.detail.unwrap().contains("64 chars"));
    }

    #[test]
    fn check_token_format_fails_on_missing_token() {
        let t = make_target("hetzner-cloud", None);
        let c = check_token_format(&t);
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.hint.is_some());
    }

    #[test]
    fn check_token_ping_warns_when_no_ping_flag_set() {
        let t = make_target("hetzner-cloud", Some("a".repeat(64)));
        let c = check_token_ping(&t, true);
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.detail.unwrap().contains("--no-ping"));
    }

    fn make_target(provider: &str, token: Option<String>) -> Target {
        Target {
            name: "t".into(),
            config: cli_core::target::TargetConfig {
                provider: provider.into(),
                ..Default::default()
            },
            credentials: cli_core::target::TargetCredentials {
                hetzner_token: token,
            },
        }
    }
}
