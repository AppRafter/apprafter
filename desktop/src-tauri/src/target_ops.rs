// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The D.3 commands' bodies, one plain function each, over the core (overview §3.12.1). The
//! `#[tauri::command]` wrappers in `commands.rs` only move them onto the blocking pool, so these
//! are what the tests drive.
//!
//! Three kinds: a plain read answers at once (local files, or probes the core bounds); a read
//! that waits on the network or on tools is an operation ([`read`]) the page follows and can
//! cancel; a mutation is a plan ([`register`]) whose class decides its confirmation. Every
//! target is named (`TargetRef::named`): the desktop never acts on the CLI's default
//! (tests/core_guard.rs). A token arrives as a `SecretString` and leaves only to the provider.
//!
//! What runs tools (the toolchain, doctor) takes the shell's tool context
//! ([`Shell::tool_context`]): on macOS its search path may still be on its way from the login
//! shell, and is waited for there — on the read's own thread for doctor — never on the way to
//! the window.
//!
//! A cancelled plan ends the same way whichever way the core says it: `Ok(Outcome::Cancelled)`
//! (cancelled before the store lock) and `Err(CoreError::Cancelled)` (cancelled during a
//! network step) both end the operation cancelled, with what it cleaned and left when the core
//! names them ([`crate::ops::Executor`]).

use std::path::PathBuf;
use std::sync::Arc;

use apprafter_core::doctor::{self, DoctorArgs, DoctorTarget};
use apprafter_core::machine::{self, CatalogueSource};
use apprafter_core::session::{self, WhoamiReport};
use apprafter_core::ssh::{self, SshKeyCandidate, SshKeyInfo};
use apprafter_core::target::{
    self, AddArgs, MachineChoice, RenewArgs, TargetListReport, TargetReport,
};
use apprafter_core::tools::{self, ToolchainReport};
use apprafter_core::{
    CancellationToken, Context, CoreError, CoreResult, Outcome, Plan, Reporter, SecretString,
    TargetRef,
};
use apprafter_desktop_ipc::{
    CatalogueSourceArg, DraftId, OpId, PlanView, TargetAddArgs, TokenVerified,
};
use serde::Serialize;

use crate::app::Shell;
use crate::errors::DesktopError;
use crate::ops::{Executor, PlanParts};

/// A report as the page receives it. A derived `Serialize` with string keys cannot fail; if it
/// ever did, the panic is the operation's `internal` error.
fn to_json<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("a core report serialises")
}

fn json_outcome<T: Serialize>(outcome: Outcome<T>) -> Outcome<serde_json::Value> {
    match outcome {
        Outcome::Completed { result } => Outcome::Completed {
            result: to_json(&result),
        },
        Outcome::Cancelled { cleaned, left } => Outcome::Cancelled { cleaned, left },
    }
}

/// Keep `plan` in the operation manager, `run` (the core's `execute_*`) its executor over a
/// clone of the shell's context; the view is all the page gets. A destructive plan asks the
/// owner inside `op_execute`, naming `verb` and `target`.
pub(crate) fn register<P: Send + 'static, T: Serialize>(
    shell: &Shell,
    plan: Plan<P>,
    target: String,
    verb: &str,
    run: impl FnOnce(&Context, Plan<P>, &dyn Reporter, &CancellationToken) -> CoreResult<Outcome<T>>
        + Send
        + 'static,
) -> PlanView {
    let parts = PlanParts {
        target: Some(target),
        changes: plan.changes.clone(),
        ..PlanParts::new(plan.class, plan.title.clone(), verb)
    };
    let ctx = shell.context.clone();
    let executor: Executor =
        Box::new(move |reporter, cancel| run(&ctx, plan, reporter, cancel).map(json_outcome));
    shell.ops.register_plan(parts, executor)
}

/// Start `run` as a read the page follows (`op_subscribe`) and can cancel, over a clone of the
/// shell's context.
pub(crate) fn read<T: Serialize>(
    shell: &Shell,
    title: String,
    target: Option<String>,
    run: impl FnOnce(&Context, &dyn Reporter, &CancellationToken) -> CoreResult<T> + Send + 'static,
) -> Result<OpId, DesktopError> {
    let ctx = shell.context.clone();
    read_in(shell, title, target, move || ctx, run)
}

/// [`read`], its context made on the read's own thread by `context` — for what runs tools,
/// [`Shell::tool_context_later`], so the command answers before the tool search path is known.
fn read_in<T: Serialize>(
    shell: &Shell,
    title: String,
    target: Option<String>,
    context: impl FnOnce() -> Context + Send + 'static,
    run: impl FnOnce(&Context, &dyn Reporter, &CancellationToken) -> CoreResult<T> + Send + 'static,
) -> Result<OpId, DesktopError> {
    let executor: Executor = Box::new(move |reporter, cancel| {
        let ctx = context();
        let report = run(&ctx, reporter, cancel)?;
        Ok(Outcome::Completed {
            result: to_json(&report),
        })
    });
    shell.start_read(title, target, executor)
}

pub fn target_list(shell: &Shell) -> Result<TargetListReport, DesktopError> {
    Ok(target::list(&shell.context)?)
}

pub fn target_show(shell: &Shell, name: &str) -> Result<TargetReport, DesktopError> {
    let named = TargetRef::named(&shell.context, name)?;
    Ok(target::show(&shell.context, &named)?)
}

pub fn ssh_key_candidates(shell: &Shell) -> Result<Vec<SshKeyCandidate>, DesktopError> {
    Ok(ssh::public_key_candidates(&shell.context)?)
}

/// The key a typed path names, `~/` expanded against the context's home as a shell would for
/// the CLI (the field suggests `~/.ssh/id_ed25519.pub`); its `path` is the expanded one, which is
/// what a plan is then given.
pub fn ssh_key_inspect(shell: &Shell, path: &str) -> Result<SshKeyInfo, DesktopError> {
    Ok(ssh::inspect_key(
        &shell.context,
        &typed_key_path(shell, path),
    )?)
}

/// A key path the page typed, as the core is given it: `~/` expanded against the context's home.
fn typed_key_path(shell: &Shell, path: &str) -> PathBuf {
    ssh::expand_tilde(path, shell.context.home_dir())
}

/// Bounded by the core: the probes run concurrently, each killed after 5 s (R14) — and, the
/// first time on macOS, by the wait for the login shell's `PATH` ([`Shell::tool_context`]).
pub fn toolchain_status(shell: &Shell) -> Result<ToolchainReport, DesktopError> {
    Ok(tools::toolchain(
        &shell.tool_context(),
        &CancellationToken::new(),
    )?)
}

/// No ping (R13): the About row opens at once; `start_whoami` verifies on request.
pub fn whoami(shell: &Shell) -> Result<WhoamiReport, DesktopError> {
    let ctx = shell.context.clone().with_no_ping(true);
    Ok(session::whoami(&ctx, &CancellationToken::new())?)
}

/// Verify `token` with `provider` (a read): its result names the draft the token now waits in,
/// never the token. A lock while it runs keeps nothing ([`crate::drafts::DraftEpoch`]), nor does
/// a cancel: the provider's request cannot be interrupted, so the token is looked at again once
/// it answers, and a cancelled read drops the token with it.
pub fn start_verify_token(
    shell: &Shell,
    provider: String,
    token: SecretString,
) -> Result<OpId, DesktopError> {
    let epoch = shell.drafts.epoch();
    let drafts = Arc::clone(&shell.drafts);
    let title = format!("Verify the {provider} token");
    read(shell, title, None, move |ctx, _, cancel| {
        let check = apprafter_core::provider::verify_token(ctx, &provider, &token, cancel)?;
        // Cancelled while the provider answered (the page cancelled, or the wizard closed): no
        // page is waiting for this draft.
        cancel.check()?;
        // A lock since the verify started dropped every draft; this one is not kept either.
        let draft_id = drafts
            .insert(epoch, provider, token)
            .ok_or(CoreError::Cancelled)?;
        Ok(TokenVerified {
            draft_id,
            elapsed_ms: check.elapsed_ms,
        })
    })
}

/// The regions and machines a picker offers, read with a draft's token (the add wizard) or a
/// stored target's. An unknown draft or target is refused here, before any read starts.
pub fn start_machine_catalogue(
    shell: &Shell,
    source: CatalogueSourceArg,
) -> Result<OpId, DesktopError> {
    match source {
        CatalogueSourceArg::Draft { draft_id } => {
            let (provider, token) = shell.drafts.get(draft_id)?;
            read(
                shell,
                "Machine catalogue".into(),
                None,
                move |ctx, _, cancel| {
                    machine::catalogue(
                        ctx,
                        CatalogueSource::Token {
                            provider: &provider,
                            token: &token,
                        },
                        cancel,
                    )
                },
            )
        }
        CatalogueSourceArg::Target { name } => {
            let named = TargetRef::named(&shell.context, &name)?;
            read(
                shell,
                format!("Machine catalogue · {name}"),
                Some(name),
                move |ctx, _, cancel| {
                    machine::catalogue(ctx, CatalogueSource::Target(&named), cancel)
                },
            )
        }
    }
}

pub fn start_region_latencies(shell: &Shell, regions: Vec<String>) -> Result<OpId, DesktopError> {
    read(
        shell,
        "Region latency".into(),
        None,
        move |ctx, _, cancel| machine::region_latencies(ctx, &regions, cancel),
    )
}

/// Doctor on the target called `name` (`DoctorTarget::Named`: a name the store does not hold is
/// a FAIL row, not an error), with the tools the app has learned of. Its stages count three for
/// a target that exists (Target, Cluster, This computer) and two otherwise.
pub fn start_doctor(shell: &Shell, name: String) -> Result<OpId, DesktopError> {
    let title = format!("Doctor · {name}");
    let args = DoctorArgs {
        target: DoctorTarget::Named(name.clone()),
    };
    read_in(
        shell,
        title,
        Some(name),
        shell.tool_context_later(),
        move |ctx, reporter, cancel| doctor::run(ctx, args, reporter, cancel),
    )
}

/// whoami with a ping of the CLI default's stored token.
pub fn start_whoami(shell: &Shell) -> Result<OpId, DesktopError> {
    read(
        shell,
        "Verify the CLI default's token".into(),
        None,
        |ctx, _, cancel| session::whoami(ctx, cancel),
    )
}

/// Plan adding a target with the token a verify left in `args.draft_id` (Bounded). The draft
/// must be live and of `args.provider`; it is taken only once the plan is made, so a refused
/// plan leaves it for the corrected form. The GUI never overwrites a target (decision 8).
pub fn plan_target_add(shell: &Shell, args: TargetAddArgs) -> Result<PlanView, DesktopError> {
    let (provider, token) = shell.drafts.get(args.draft_id)?;
    if provider != args.provider {
        return Err(DesktopError::Internal(format!(
            "draft {} was verified for {provider}, not {}",
            args.draft_id.0, args.provider
        )));
    }
    let name = args.name.clone();
    let plan = target::plan_add(
        &shell.context,
        AddArgs {
            name: args.name,
            provider,
            token,
            ssh_key: args.ssh_key.map(|p| typed_key_path(shell, &p)),
            region: args.region,
            tier: args.tier,
            cluster_name: None,
            server_type: args.server_type,
            force: false, // decision 8: the GUI never overwrites a target
        },
    )?;
    // Planned: the token is the plan's now and goes with it (run, discarded, expired, locked).
    drop(shell.drafts.take(args.draft_id)?);
    Ok(register(shell, plan, name, "add", target::execute_add))
}

/// Plan renewing `name` (Bounded): a new token, a new SSH key path, or both (`target add
/// <name> --renew [--token <t>] [--ssh-key <path>]`; the Target screen's Renew token and its SSH
/// key row). The core changes what differs from what is stored: a key alone (`token: None`)
/// keeps the credentials and asks the provider nothing; a new token is checked with the provider
/// when the plan runs, and only then saved. An unreadable key, or a renewal that would change
/// nothing, is refused before any plan. The key path is typed: `~/` expands against the home.
pub fn plan_target_renew(
    shell: &Shell,
    name: &str,
    token: Option<SecretString>,
    ssh_key: Option<String>,
) -> Result<PlanView, DesktopError> {
    let named = TargetRef::named(&shell.context, name)?;
    let ssh_key = ssh_key.map(|p| typed_key_path(shell, &p));
    let verb = if token.is_some() {
        "renew the token of"
    } else {
        "change the SSH key of"
    };
    let plan = target::plan_renew(&shell.context, &named, RenewArgs { token, ssh_key })?;
    Ok(register(
        shell,
        plan,
        name.into(),
        verb,
        target::execute_renew,
    ))
}

/// Make `name` the CLI's default (Reversible: the page runs it at once).
pub fn plan_target_use(shell: &Shell, name: &str) -> Result<PlanView, DesktopError> {
    let named = TargetRef::named(&shell.context, name)?;
    let plan = target::plan_use(&shell.context, &named)?;
    Ok(register(
        shell,
        plan,
        name.into(),
        "make default",
        target::execute_use,
    ))
}

pub fn plan_target_rename(shell: &Shell, from: &str, to: &str) -> Result<PlanView, DesktopError> {
    let named = TargetRef::named(&shell.context, from)?;
    let plan = target::plan_rename(&shell.context, &named, to)?;
    Ok(register(
        shell,
        plan,
        from.into(),
        "rename",
        target::execute_rename,
    ))
}

/// Remove `name` from this computer (Destructive: the owner confirms inside `op_execute`).
pub fn plan_target_remove(shell: &Shell, name: &str) -> Result<PlanView, DesktopError> {
    let named = TargetRef::named(&shell.context, name)?;
    let plan = target::plan_remove(&shell.context, &named)?;
    Ok(register(
        shell,
        plan,
        name.into(),
        "remove",
        target::execute_remove,
    ))
}

/// Change the machine of `name` (Bounded); a provisioned target is refused before any plan.
pub fn plan_target_machine(
    shell: &Shell,
    name: &str,
    sku: String,
    region: Option<String>,
) -> Result<PlanView, DesktopError> {
    let named = TargetRef::named(&shell.context, name)?;
    let plan = target::plan_machine(&shell.context, &named, MachineChoice { sku, region })?;
    Ok(register(
        shell,
        plan,
        name.into(),
        "change the machine of",
        target::execute_machine,
    ))
}

/// The page is done with a draft (the wizard closed); an unknown id is no error.
pub fn draft_discard(shell: &Shell, draft_id: DraftId) {
    shell.drafts.discard(draft_id);
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    use apprafter_core::{
        ChangeAction, Context, CoreError, Outcome, PathSource, Plan, PlanClass, SecretString,
    };
    use apprafter_desktop_ipc::{
        errors, CatalogueSourceArg, DraftId, OpEvent, OpId, OpState, TargetAddArgs,
    };
    use cli_core::target::{Target, TargetConfig, TargetCredentials};
    use serde_json::{json, Value};

    use super::*;
    use crate::app::Shell;
    use crate::auth::{AuthPurpose, FakeAuthenticator};
    use crate::env::ToolSearchPath;
    use crate::ops::{EventSink, SystemClock};
    use crate::settings::SettingsStore;

    struct Store {
        _dir: tempfile::TempDir,
        shell: Arc<Shell>,
        auth: Arc<FakeAuthenticator>,
    }

    /// A shell over a temp store holding `names` (hetzner-cloud, no token); `default` the CLI's.
    fn store(names: &[&str], default: Option<&str>) -> Store {
        store_with_tools(
            names,
            default,
            ToolSearchPath::known(OsString::new(), PathSource::Explicit),
        )
    }

    /// [`store`], the shell learning the tool search path through `tools`.
    fn store_with_tools(names: &[&str], default: Option<&str>, tools: ToolSearchPath) -> Store {
        store_on(names, default, tools, "http://127.0.0.1:9")
    }

    /// [`store`], its provider API at `api_base`.
    fn store_on(
        names: &[&str],
        default: Option<&str>,
        tools: ToolSearchPath,
        api_base: &str,
    ) -> Store {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::for_desktop(dir.path().join("store"), api_base);
        for name in names {
            let target = Target {
                name: name.to_string(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: TargetCredentials::default(),
            };
            cli_core::save_target(&ctx.store(), &target).unwrap();
        }
        if let Some(name) = default {
            let config = cli_core::GlobalConfig {
                active_target: name.into(),
                version: cli_core::TARGET_STORE_VERSION,
            };
            cli_core::save_global_config(&ctx.store(), &config).unwrap();
        }
        let auth = Arc::new(FakeAuthenticator::new());
        let settings = SettingsStore::load(dir.path(), &SystemClock);
        let shell = Shell::new(
            settings,
            auth.clone(),
            Arc::new(SystemClock),
            ctx,
            tools,
            false,
            |_| {},
        );
        Store {
            _dir: dir,
            shell,
            auth,
        }
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<OpEvent>>);

    impl EventSink for Recorder {
        fn send(&self, event: &OpEvent) -> bool {
            self.0.lock().unwrap().push(event.clone());
            true
        }
        fn webview(&self) -> &str {
            "main"
        }
    }

    /// Every event of the operation, its last one the final (replay first, then live).
    fn followed(shell: &Shell, id: OpId) -> Vec<OpEvent> {
        let sink = Arc::new(Recorder::default());
        let replay = shell.ops.subscribe(id, sink.clone()).unwrap().replay;
        sink.0.lock().unwrap().splice(0..0, replay);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let events = sink.0.lock().unwrap().clone();
            if let Some(OpEvent::Finished { .. } | OpEvent::Failed { .. }) = events.last() {
                return events;
            }
            assert!(Instant::now() < deadline, "operation {id:?} did not end");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The operation's last event, once it has one.
    fn ended(shell: &Shell, id: OpId) -> OpEvent {
        followed(shell, id).pop().unwrap()
    }

    fn result_of(event: OpEvent) -> Value {
        match event {
            OpEvent::Finished {
                outcome: Outcome::Completed { result },
            } => result,
            other => panic!("not completed: {other:?}"),
        }
    }

    fn run(shell: &Shell, view: &apprafter_desktop_ipc::PlanView) -> Value {
        shell
            .execute(view.op_id, Arc::new(Recorder::default()))
            .unwrap();
        result_of(ended(shell, view.op_id))
    }

    fn a_token(c: char) -> SecretString {
        SecretString::new(c.to_string().repeat(64))
    }

    /// A key path as the page sends it.
    fn path_of(path: &std::path::Path) -> String {
        path.display().to_string()
    }

    /// A provider API on loopback: each request it gets is told on `asked`, and answered — 200,
    /// no locations — once `answer` lets it, one per message.
    struct Api {
        base: String,
        asked: mpsc::Receiver<()>,
        answer: mpsc::Sender<()>,
    }

    fn api() -> Api {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (asked_tx, asked) = mpsc::channel();
        let (answer, answer_rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = asked_tx.send(());
                if answer_rx.recv_timeout(Duration::from_secs(30)).is_err() {
                    return;
                }
                let body = br#"{"locations":[]}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
        });
        Api {
            base,
            asked,
            answer,
        }
    }

    fn unlocked_store_on(names: &[&str], api: &Api) -> Store {
        let s = store_on(
            names,
            None,
            ToolSearchPath::known(OsString::new(), PathSource::Explicit),
            &api.base,
        );
        s.shell.unlock().unwrap();
        s
    }

    #[test]
    fn a_verified_token_waits_as_a_draft_and_the_add_plan_takes_it() {
        let api = api();
        let s = unlocked_store_on(&[], &api);
        let id = start_verify_token(&s.shell, "hetzner-cloud".into(), a_token('k')).unwrap();
        api.asked
            .recv_timeout(Duration::from_secs(10))
            .expect("the provider was asked");
        api.answer.send(()).unwrap();
        let verified = result_of(ended(&s.shell, id));
        let keys: Vec<&String> = verified.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["draftId", "elapsedMs"],
            "the draft, never the token: {verified}"
        );
        let draft = DraftId(verified["draftId"].as_u64().unwrap());
        assert_eq!(
            s.shell.drafts.get(draft).unwrap().1.expose(),
            "k".repeat(64)
        );
        let view = plan_target_add(
            &s.shell,
            TargetAddArgs {
                name: "prod".into(),
                provider: "hetzner-cloud".into(),
                draft_id: draft,
                ssh_key: None,
                region: None,
                tier: None,
                server_type: None,
            },
        )
        .unwrap();
        assert_eq!(view.title, "Add target prod");
        assert!(matches!(
            s.shell.drafts.get(draft),
            Err(DesktopError::DraftNotFound { .. })
        ));
    }

    /// A lock while the provider is being asked: the token it verified is not kept, and the
    /// read ends cancelled — the epoch was taken when the verify started.
    #[test]
    fn a_lock_while_a_verify_runs_keeps_no_draft() {
        let api = api();
        let s = unlocked_store_on(&[], &api);
        let id = start_verify_token(&s.shell, "hetzner-cloud".into(), a_token('k')).unwrap();
        api.asked
            .recv_timeout(Duration::from_secs(10))
            .expect("the provider was asked");
        assert!(s.shell.lock_now().locked);
        api.answer.send(()).unwrap();
        assert_eq!(
            ended(&s.shell, id),
            OpEvent::Finished {
                outcome: Outcome::Cancelled {
                    cleaned: Vec::new(),
                    left: Vec::new()
                }
            }
        );
        assert!(format!("{:?}", s.shell.drafts).contains("drafts: 0"));
    }

    /// D.3d review #4: the page cancels (or closes the wizard) while the provider is being
    /// asked. The request cannot be interrupted and answers 200; the read still ends cancelled
    /// and the verified token is not kept as a draft no page knows of.
    #[test]
    fn a_verify_cancelled_while_the_provider_answers_keeps_no_draft() {
        let api = api();
        let s = unlocked_store_on(&[], &api);
        let id = start_verify_token(&s.shell, "hetzner-cloud".into(), a_token('k')).unwrap();
        api.asked
            .recv_timeout(Duration::from_secs(10))
            .expect("the provider was asked");
        s.shell.ops.cancel(id).unwrap();
        api.answer.send(()).unwrap();
        assert_eq!(
            ended(&s.shell, id),
            OpEvent::Finished {
                outcome: Outcome::Cancelled {
                    cleaned: Vec::new(),
                    left: Vec::new()
                }
            }
        );
        assert!(format!("{:?}", s.shell.drafts).contains("drafts: 0"));
    }

    /// Decision 8: the GUI never overwrites a target; the draft stays for another name.
    #[test]
    fn the_add_plan_never_overwrites_a_target() {
        let s = store(&["prod"], None);
        let draft = s
            .shell
            .drafts
            .insert(s.shell.drafts.epoch(), "hetzner-cloud".into(), a_token('k'))
            .unwrap();
        let ui = plan_target_add(
            &s.shell,
            TargetAddArgs {
                name: "prod".into(),
                provider: "hetzner-cloud".into(),
                draft_id: draft,
                ssh_key: None,
                region: None,
                tier: None,
                server_type: None,
            },
        )
        .unwrap_err()
        .to_ui();
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::exists"));
        assert!(s.shell.drafts.get(draft).is_ok());
    }

    #[test]
    fn reads_list_and_show_and_an_unknown_name_is_not_found() {
        let s = store(&["prod", "staging"], Some("prod"));
        let list = target_list(&s.shell).unwrap();
        assert_eq!(
            list.targets
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["prod", "staging"]
        );
        assert_eq!(target_show(&s.shell, "staging").unwrap().name, "staging");
        let ui = target_show(&s.shell, "ghost").unwrap_err().to_ui();
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::not_found"));
        assert_eq!(ui.fields["available"], json!(["prod", "staging"]));
    }

    #[test]
    fn use_is_reversible_and_runs_without_a_gesture() {
        let s = store(&["prod", "staging"], Some("prod"));
        let view = plan_target_use(&s.shell, "staging").unwrap();
        assert_eq!(
            (view.class, view.target.as_deref()),
            (PlanClass::Reversible, Some("staging"))
        );
        let out = run(&s.shell, &view);
        assert_eq!(
            out,
            json!({ "name": "staging", "pointer": { "from": "prod", "to": "staging" } })
        );
        assert!(s.auth.asked().is_empty());
        let pointer = cli_core::resolve_active_target_name(&s.shell.context.store(), None).unwrap();
        assert_eq!(pointer.as_deref(), Some("staging"));
    }

    #[test]
    fn rename_is_bounded_and_takes_the_default_along() {
        let s = store(&["prod"], Some("prod"));
        let view = plan_target_rename(&s.shell, "prod", "prod-eu").unwrap();
        assert_eq!(view.class, PlanClass::Bounded);
        assert!(s.auth.asked().is_empty());
        let out = run(&s.shell, &view);
        assert_eq!(out["to"], "prod-eu");
        assert_eq!(
            out["cliDefault"],
            json!({ "from": "prod", "to": "prod-eu" })
        );
        assert!(s.auth.asked().is_empty(), "a bounded plan asks nobody");
        let ui = plan_target_rename(&s.shell, "prod-eu", "bad name")
            .unwrap_err()
            .to_ui();
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::invalid_name"));
    }

    #[test]
    fn remove_is_destructive_and_asks_the_owner_to_remove_this_target() {
        let s = store(&["lab", "prod"], Some("prod"));
        let view = plan_target_remove(&s.shell, "prod").unwrap();
        assert_eq!(view.class, PlanClass::Destructive);
        assert!(view
            .changes
            .iter()
            .any(|c| c.kind == "Target" && c.action == ChangeAction::Delete));
        let out = run(&s.shell, &view);
        assert_eq!(out["cliDefault"], json!({ "from": "prod", "to": "lab" }));
        assert_eq!(
            s.auth.asked(),
            vec![AuthPurpose::Confirm {
                target: Some("prod".into()),
                verb: "remove".into()
            }]
        );
        assert!(!s.shell.context.store().target_dir("prod").exists());
    }

    #[test]
    fn machine_on_a_provisioned_target_is_refused_before_any_plan() {
        let s = store(&["prod"], None);
        let state = s.shell.context.config_root().join("state/prod/.apprafter");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("state.json"),
            r#"{"hetzner_cloud":{"server_id":4711,"server_name":"prod-1","server_type":"cpx22"}}"#,
        )
        .unwrap();
        let ui = plan_target_machine(&s.shell, "prod", "cx32".into(), None)
            .unwrap_err()
            .to_ui();
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::provisioned"));
        assert_eq!(ui.fields["serverId"], json!(4711));
        assert!(s.shell.ops.list().is_empty());
    }

    #[test]
    fn machine_on_a_target_without_a_server_is_a_bounded_plan() {
        let s = store(&["prod"], None);
        let view = plan_target_machine(&s.shell, "prod", "cx32".into(), None).unwrap();
        assert_eq!(
            (view.class, view.target.as_deref()),
            (PlanClass::Bounded, Some("prod"))
        );
    }

    #[test]
    fn renew_is_bounded_and_a_malformed_token_is_refused_before_any_plan() {
        let s = store(&["prod"], None);
        let view = plan_target_renew(&s.shell, "prod", Some(a_token('k')), None).unwrap();
        assert_eq!(view.class, PlanClass::Bounded);
        let ui = plan_target_renew(&s.shell, "prod", Some(SecretString::new("short")), None)
            .unwrap_err()
            .to_ui();
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::invalid_token"));
    }

    /// The Target screen's SSH key row (WI-452): `target add <name> --renew --ssh-key <path>`
    /// as a plan — the key path reaches the plan's changes and the save, with the new token.
    #[test]
    fn renew_carries_a_new_ssh_key_to_the_plan_and_the_save() {
        let api = api();
        let s = unlocked_store_on(&["prod"], &api);
        let key = s._dir.path().join("id_ed25519.pub");
        std::fs::write(&key, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 alex@workstation\n").unwrap();
        let view =
            plan_target_renew(&s.shell, "prod", Some(a_token('k')), Some(path_of(&key))).unwrap();
        assert_eq!(view.class, PlanClass::Bounded);
        assert!(
            view.changes.iter().any(|c| c.kind == "Target"
                && c.action == ChangeAction::Update
                && c.detail
                    .as_deref()
                    .is_some_and(|d| d.starts_with("ssh key: not set → "))),
            "{:?}",
            view.changes
        );
        s.shell
            .execute(view.op_id, Arc::new(Recorder::default()))
            .unwrap();
        api.asked
            .recv_timeout(Duration::from_secs(10))
            .expect("the provider was asked");
        api.answer.send(()).unwrap();
        assert_eq!(
            result_of(ended(&s.shell, view.op_id))["sshKeyChanged"],
            json!(true)
        );
        let saved = cli_core::load_target(&s.shell.context.store(), "prod").unwrap();
        assert_eq!(saved.config.ssh_key_path.as_deref(), Some(key.as_path()));
    }

    #[test]
    fn renew_with_an_unreadable_ssh_key_is_refused_before_any_plan() {
        let s = store(&["prod"], None);
        let missing = s._dir.path().join("nothing-here.pub");
        let ui = plan_target_renew(
            &s.shell,
            "prod",
            Some(a_token('k')),
            Some(path_of(&missing)),
        )
        .unwrap_err()
        .to_ui();
        assert_eq!(
            ui.code.as_deref(),
            Some("apprafter::target::ssh_key_unreadable")
        );
    }

    /// D.3d review #2/#7/#11/#16: the SSH key field suggests `~/.ssh/id_ed25519.pub`, and in the
    /// CLI the shell expands it. A typed `~/` path expands against the context's home: inspected,
    /// it is found and answered absolute, and the renew and add plans save the absolute path.
    #[test]
    fn a_typed_key_path_expands_against_the_home() {
        let api = api();
        let s = unlocked_store_on(&["prod"], &api);
        let home = s._dir.path().join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        let key = home.join(".ssh").join("work.pub");
        std::fs::write(&key, "ssh-ed25519 AAAA alex@work\n").unwrap();
        let shell = Shell::new(
            SettingsStore::load(s._dir.path(), &SystemClock),
            Arc::new(FakeAuthenticator::new()),
            Arc::new(SystemClock),
            s.shell.context.clone().with_home_dir(Some(home.clone())),
            ToolSearchPath::known(OsString::new(), PathSource::Explicit),
            false,
            |_| {},
        );
        shell.unlock().unwrap();
        let info = ssh_key_inspect(&shell, "~/.ssh/work.pub").unwrap();
        assert_eq!(
            (info.path.as_str(), info.exists, info.algo.as_deref()),
            (path_of(&key).as_str(), true, Some("ssh-ed25519"))
        );
        let view = plan_target_renew(&shell, "prod", None, Some("~/.ssh/work.pub".into())).unwrap();
        run(&shell, &view);
        let saved = cli_core::load_target(&shell.context.store(), "prod").unwrap();
        assert_eq!(saved.config.ssh_key_path.as_deref(), Some(key.as_path()));
        let draft = shell
            .drafts
            .insert(shell.drafts.epoch(), "hetzner-cloud".into(), a_token('k'))
            .unwrap();
        let view = plan_target_add(
            &shell,
            TargetAddArgs {
                name: "lab".into(),
                provider: "hetzner-cloud".into(),
                draft_id: draft,
                ssh_key: Some("~/.ssh/work.pub".into()),
                region: None,
                tier: None,
                server_type: None,
            },
        )
        .unwrap();
        shell
            .execute(view.op_id, Arc::new(Recorder::default()))
            .unwrap();
        api.asked
            .recv_timeout(Duration::from_secs(10))
            .expect("the add pings the token");
        api.answer.send(()).unwrap();
        result_of(ended(&shell, view.op_id));
        let saved = cli_core::load_target(&shell.context.store(), "lab").unwrap();
        assert_eq!(saved.config.ssh_key_path.as_deref(), Some(key.as_path()));
    }

    /// GOTCHA-149: a private key is the file next to the `.pub`, and a provider is sent whatever
    /// the key file holds. Inspected, it has no type and says why; the renew and add plans refuse
    /// it before any plan, by name, and the draft stays for the corrected form.
    #[test]
    fn a_private_key_is_inspected_as_one_and_no_plan_takes_it() {
        let s = store(&["prod"], None);
        let key = s._dir.path().join("id_ed25519");
        std::fs::write(
            &key,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaA==\n-----END OPENSSH PRIVATE KEY-----\n",
        )
        .unwrap();
        let info = ssh_key_inspect(&s.shell, key.to_str().unwrap()).unwrap();
        assert_eq!(
            (info.exists, info.algo, info.problem),
            (true, None, Some(ssh::SshKeyProblem::PrivateKey))
        );
        let refused = |ui: apprafter_core::UiError| {
            assert_eq!(
                ui.code.as_deref(),
                Some("apprafter::target::ssh_key_not_public")
            );
            assert_eq!(ui.fields["privateKey"], json!(true));
            assert!(!ui.message.contains("b3Blbn"), "{}", ui.message);
        };
        refused(
            plan_target_renew(&s.shell, "prod", None, Some(path_of(&key)))
                .unwrap_err()
                .to_ui(),
        );
        let draft = s
            .shell
            .drafts
            .insert(s.shell.drafts.epoch(), "hetzner-cloud".into(), a_token('k'))
            .unwrap();
        refused(
            plan_target_add(
                &s.shell,
                TargetAddArgs {
                    name: "lab".into(),
                    provider: "hetzner-cloud".into(),
                    draft_id: draft,
                    ssh_key: Some(key.display().to_string()),
                    region: None,
                    tier: None,
                    server_type: None,
                },
            )
            .unwrap_err()
            .to_ui(),
        );
        assert!(
            s.shell.drafts.get(draft).is_ok(),
            "a refused plan takes nothing"
        );
        assert!(s.shell.ops.list().is_empty());
    }

    /// WI-452: the SSH key row's renewal carries no token: the plan lists only the key, the
    /// run asks the provider nothing and keeps the credentials, and the result says so.
    #[test]
    fn a_key_only_renew_asks_the_provider_nothing_and_keeps_the_credentials() {
        let api = api();
        let s = unlocked_store_on(&["prod"], &api);
        let key = s._dir.path().join("id_ed25519.pub");
        std::fs::write(&key, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 alex@workstation\n").unwrap();
        let view = plan_target_renew(&s.shell, "prod", None, Some(path_of(&key))).unwrap();
        assert_eq!(
            (view.class, view.title.as_str()),
            (PlanClass::Bounded, "Change the SSH key of prod")
        );
        assert_eq!(
            view.changes
                .iter()
                .map(|c| (c.kind.as_str(), c.action))
                .collect::<Vec<_>>(),
            [("Target", ChangeAction::Update)]
        );
        let result = run(&s.shell, &view);
        assert!(
            api.asked.try_recv().is_err(),
            "no provider request for a key-only renewal"
        );
        assert_eq!(
            (result["token"].clone(), result["sshKeyChanged"].clone()),
            (Value::Null, json!(true))
        );
        let saved = cli_core::load_target(&s.shell.context.store(), "prod").unwrap();
        assert_eq!(saved.config.ssh_key_path.as_deref(), Some(key.as_path()));
        assert_eq!(saved.credentials.hetzner_token, None);
    }

    #[test]
    fn a_renew_with_nothing_to_change_is_refused_before_any_plan() {
        let s = store(&["prod"], None);
        let ui = plan_target_renew(&s.shell, "prod", None, None)
            .unwrap_err()
            .to_ui();
        assert_eq!(
            ui.code.as_deref(),
            Some("apprafter::target::renew_nothing_to_change")
        );
        assert_eq!(ui.fields["name"], json!("prod"));
        assert!(s.shell.ops.list().is_empty());
    }

    #[test]
    fn a_malformed_token_fails_its_read_and_leaves_no_draft() {
        let s = store(&[], None);
        let id = start_verify_token(&s.shell, "hetzner-cloud".into(), SecretString::new("short"))
            .unwrap();
        match ended(&s.shell, id) {
            OpEvent::Failed { error } => assert_eq!(
                error.code.as_deref(),
                Some("apprafter::target::invalid_token")
            ),
            other => panic!("{other:?}"),
        }
        assert!(format!("{:?}", s.shell.drafts).contains("drafts: 0"));
        let listed = s.shell.ops.list();
        assert_eq!(listed[0].title, "Verify the hetzner-cloud token");
    }

    #[test]
    fn the_add_plan_needs_a_live_draft_of_its_provider_and_takes_it() {
        let s = store(&[], None);
        let args = |draft| TargetAddArgs {
            name: "prod".into(),
            provider: "hetzner-cloud".into(),
            draft_id: draft,
            ssh_key: None,
            region: None,
            tier: None,
            server_type: None,
        };
        let ui = plan_target_add(&s.shell, args(DraftId(9)))
            .unwrap_err()
            .to_ui();
        assert_eq!(ui.code.as_deref(), Some(errors::DRAFT_NOT_FOUND));
        let aws = s
            .shell
            .drafts
            .insert(s.shell.drafts.epoch(), "aws".into(), a_token('k'))
            .unwrap();
        let ui = plan_target_add(&s.shell, args(aws)).unwrap_err().to_ui();
        assert_eq!(ui.code.as_deref(), Some(errors::INTERNAL));
        let ok = s
            .shell
            .drafts
            .insert(s.shell.drafts.epoch(), "hetzner-cloud".into(), a_token('k'))
            .unwrap();
        // A plan the core refuses leaves the draft for the corrected form.
        let ui = plan_target_add(
            &s.shell,
            TargetAddArgs {
                name: "bad name".into(),
                ..args(ok)
            },
        )
        .unwrap_err()
        .to_ui();
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::invalid_name"));
        assert!(
            s.shell.drafts.get(ok).is_ok(),
            "a refused plan takes nothing"
        );
        let view = plan_target_add(&s.shell, args(ok)).unwrap();
        assert_eq!(view.class, PlanClass::Bounded);
        assert!(
            matches!(
                s.shell.drafts.get(ok),
                Err(DesktopError::DraftNotFound { .. })
            ),
            "the plan took it"
        );
        let ui = start_machine_catalogue(&s.shell, CatalogueSourceArg::Draft { draft_id: ok })
            .unwrap_err()
            .to_ui();
        assert_eq!(ui.code.as_deref(), Some(errors::DRAFT_NOT_FOUND));
        let ui = start_machine_catalogue(
            &s.shell,
            CatalogueSourceArg::Target {
                name: "ghost".into(),
            },
        )
        .unwrap_err()
        .to_ui();
        assert_eq!(ui.code.as_deref(), Some("apprafter::target::not_found"));
        assert!(s.shell.ops.list().is_empty(), "no read started");
    }

    #[test]
    fn a_draft_discarded_is_gone_and_an_unknown_one_is_no_error() {
        let s = store(&[], None);
        let id = s
            .shell
            .drafts
            .insert(s.shell.drafts.epoch(), "hetzner-cloud".into(), a_token('k'))
            .unwrap();
        draft_discard(&s.shell, id);
        draft_discard(&s.shell, DraftId(999));
        assert!(matches!(
            s.shell.drafts.get(id),
            Err(DesktopError::DraftNotFound { .. })
        ));
    }

    #[test]
    fn doctor_whoami_and_latencies_are_reads_that_end_with_their_report() {
        let s = store(&["prod"], Some("prod"));
        let doctor = result_of(ended(
            &s.shell,
            start_doctor(&s.shell, "prod".into()).unwrap(),
        ));
        assert_eq!(doctor["target"], "prod");
        assert!(doctor["groups"].as_array().is_some_and(|g| !g.is_empty()));
        assert_eq!(
            result_of(ended(
                &s.shell,
                start_region_latencies(&s.shell, vec![]).unwrap()
            )),
            json!([])
        );
        let me = serde_json::to_value(whoami(&s.shell).unwrap()).unwrap();
        assert_eq!(me["identity"], "anonymous_self_hosted");
        assert_eq!(
            me["cliDefault"]["target"]["verification"],
            json!({ "status": "skipped", "reason": "no_ping" }),
            "{me}"
        );
        assert!(s
            .shell
            .ops
            .list()
            .iter()
            .any(|o| o.title == "Doctor · prod" && o.target.as_deref() == Some("prod")));
    }

    /// The D.3c hand-off: doctor runs on the named target, and counts three stages for one the
    /// store holds (Target, Cluster, This computer) and two for one it does not.
    #[test]
    fn doctor_runs_on_the_named_target_in_three_stages_or_two() {
        let s = store(&["prod", "lab"], Some("lab"));
        let stages = |events: &[OpEvent]| -> Vec<(u32, u32, String)> {
            events
                .iter()
                .filter_map(|e| match e {
                    OpEvent::Stage {
                        index,
                        total,
                        title,
                    } => Some((*index, *total, title.clone())),
                    _ => None,
                })
                .collect()
        };
        let events = followed(&s.shell, start_doctor(&s.shell, "prod".into()).unwrap());
        assert_eq!(
            stages(&events),
            [
                (1, 3, "Target".into()),
                (2, 3, "Cluster".into()),
                (3, 3, "This computer".into())
            ]
        );
        assert_eq!(
            result_of(events.last().cloned().unwrap())["target"],
            "prod",
            "not the CLI default"
        );
        let events = followed(&s.shell, start_doctor(&s.shell, "ghost".into()).unwrap());
        assert_eq!(
            stages(&events),
            [(1, 2, "Target".into()), (2, 2, "This computer".into())]
        );
        assert_eq!(
            result_of(events.last().cloned().unwrap())["target"],
            "ghost"
        );
    }

    /// WI-452: what runs tools takes the tool search path the app has learned — on macOS the
    /// login shell's, which the context the shell was built with does not have.
    #[test]
    fn the_toolchain_and_doctor_look_for_tools_where_the_app_learned_to() {
        let s = store_with_tools(
            &["prod"],
            None,
            ToolSearchPath::known(
                OsString::from("/from/the/login/shell"),
                PathSource::LoginShell,
            ),
        );
        assert!(s.shell.context.tool_search_path().is_empty());
        let report = toolchain_status(&s.shell).unwrap();
        assert_eq!(
            (report.search_path, report.search_path_source),
            (
                vec!["/from/the/login/shell".to_string()],
                PathSource::LoginShell
            )
        );
        let started = start_doctor(&s.shell, "prod".into()).unwrap();
        assert!(matches!(ended(&s.shell, started), OpEvent::Finished { .. }));
        doctor_finds_a_tool_only_the_learned_path_has();
    }

    /// D.3d review #18: doctor finds a tool that only the learned search path holds — a
    /// kubectl stand-in there, none in the context the shell was built with — so a doctor run
    /// on that start context (its tools reported missing, as on macOS before the login shell
    /// answered) fails here. Unix: the stand-in is a `/bin/sh` script; Windows runs only real
    /// executables, and its tool search is covered by the CLI's goldens.
    #[cfg(unix)]
    fn doctor_finds_a_tool_only_the_learned_path_has() {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        let kubectl = bin.path().join("kubectl");
        let call = cli_core::tools::KUBECTL.version_args.join(" ");
        // Builtins only: the probe runs it with this directory alone as its PATH (GOTCHA-104).
        std::fs::write(
            &kubectl,
            format!(
                "#!/bin/sh\ncase \"$*\" in '{call}') echo 'kubectl stand-in' ;; *) exit 2 ;; esac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&kubectl, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A sibling test's fork may hold the file open for writing until it execs (ETXTBSY):
        // run it until it starts, so doctor's probe never meets that window.
        for _ in 0..200 {
            match std::process::Command::new(&kubectl)
                .args(["version", "--client"])
                .status()
            {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => break,
            }
        }
        let s = store_with_tools(
            &["prod"],
            None,
            ToolSearchPath::known(bin.path().as_os_str().to_owned(), PathSource::LoginShell),
        );
        assert!(s.shell.context.tool_search_path().is_empty());
        let report = result_of(ended(
            &s.shell,
            start_doctor(&s.shell, "prod".into()).unwrap(),
        ));
        let row = report["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == "this_computer")
            .and_then(|g| g["checks"].as_array())
            .and_then(|checks| checks.iter().find(|c| c["tool"] == "kubectl"))
            .cloned()
            .expect("a kubectl row");
        assert_eq!(
            (row["status"].clone(), row["detail"].clone()),
            (json!("pass"), json!("kubectl stand-in")),
            "{row}"
        );
    }

    #[cfg(not(unix))]
    fn doctor_finds_a_tool_only_the_learned_path_has() {}

    /// The doctor read waits for the tool search path on its own thread: the command answers
    /// while the login shell has not, and the read then goes on to its end.
    #[test]
    fn doctor_answers_before_the_login_shell_does() {
        let (release, released) = mpsc::channel::<()>();
        let tools = ToolSearchPath::probe(
            move || {
                let _ = released.recv_timeout(Duration::from_secs(600));
                Some(OsString::from("/from/the/login/shell"))
            },
            PathSource::LoginShell,
            (OsString::from("/usr/bin:/bin"), PathSource::Fallback),
            Duration::from_secs(600),
        );
        let s = store_with_tools(&["prod"], None, tools);
        let (started_tx, started) = mpsc::channel();
        {
            let shell = s.shell.clone();
            std::thread::spawn(move || {
                let _ = started_tx.send(start_doctor(&shell, "prod".into()));
            });
        }
        let answered = started.recv_timeout(Duration::from_secs(10));
        // The shell answers now, whatever happened, so nothing is left waiting.
        release.send(()).unwrap();
        let id = answered
            .expect("start_doctor waited for the login shell")
            .unwrap();
        assert!(matches!(ended(&s.shell, id), OpEvent::Finished { .. }));
    }

    /// The D.3b hand-off: a core `execute_*` says "cancelled" two ways — `Ok(Outcome::Cancelled)`
    /// before the store lock, `Err(CoreError::Cancelled)` during a network step — and the
    /// desktop ends both alike.
    #[test]
    fn a_plan_cancelled_either_way_ends_cancelled_alike() {
        let s = store(&["prod"], None);
        let plan = || Plan {
            class: PlanClass::Bounded,
            title: "Rotate the API token of prod".into(),
            changes: Vec::new(),
            payload: (),
        };
        let before_lock = register(
            &s.shell,
            plan(),
            "prod".into(),
            "renew the token of",
            |_, _, _, _| {
                Ok(Outcome::<Value>::Cancelled {
                    cleaned: Vec::new(),
                    left: Vec::new(),
                })
            },
        );
        let in_network = register(
            &s.shell,
            plan(),
            "prod".into(),
            "renew the token of",
            |_, _, _, _| Err::<Outcome<Value>, _>(CoreError::Cancelled),
        );
        let mut ends = Vec::new();
        for view in [before_lock, in_network] {
            s.shell
                .execute(view.op_id, Arc::new(Recorder::default()))
                .unwrap();
            let last = ended(&s.shell, view.op_id);
            let state = s
                .shell
                .ops
                .list()
                .into_iter()
                .find(|o| o.op_id == view.op_id)
                .map(|o| o.state);
            ends.push((last, state));
        }
        let cancelled = OpEvent::Finished {
            outcome: Outcome::Cancelled {
                cleaned: Vec::new(),
                left: Vec::new(),
            },
        };
        assert_eq!(ends[0], (cancelled.clone(), Some(OpState::Cancelled)));
        assert_eq!(ends[1], ends[0]);
    }

    /// The machine catalogue of a stored target is a read named after it.
    #[test]
    fn the_catalogue_of_a_target_is_a_read_named_after_it() {
        let s = store(&["prod"], None);
        let id = start_machine_catalogue(
            &s.shell,
            CatalogueSourceArg::Target {
                name: "prod".into(),
            },
        )
        .unwrap();
        // No token is stored: the read fails, and says why.
        assert!(matches!(ended(&s.shell, id), OpEvent::Failed { .. }));
        let listed = s.shell.ops.list();
        assert_eq!(
            (listed[0].title.as_str(), listed[0].target.as_deref()),
            ("Machine catalogue · prod", Some("prod"))
        );
    }
}
