// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The OS-authentication probe (D.2d): one window that asks the operating system to
//! authenticate the device owner through `apprafter-os-auth` and logs what it answers. It is the
//! instrument of the owner's real-hardware matrix (the research brief's §6,
//! docs/superpowers/plans/d2-research/osauth.md), and it never runs in CI: CI builds it and runs
//! its unit tests, which open no window and ask the OS nothing.
//!
//! From `desktop/`: `cargo run --example os_auth_probe`. The window has **Unlock** and
//! **Confirm** (the destructive-operation gesture), **Cancel in 3 s** (an Unlock whose token
//! trips after three seconds, to see the OS close its prompt) and **Read AuthInfo**; on Linux a
//! password field for the PAM path and **Forget the missing agent**
//! (`OsAuthenticator::reset_agent_memory`); on Windows **Prefer Windows Hello** (the `hello`
//! setting). The log lists every outcome with how long it took, the `AuthInfo` at start and
//! after each outcome, PAM's messages, and the session watch: what it hears once set up, and
//! every lock or sleep it reports. Each entry is also written to stderr.
//!
//! The page talks to the probe through a URI scheme of its own (`probe`), not the app's IPC: the
//! package's build script makes the app's command list its ACL manifest, so a command this
//! example registered would have no permission and the ACL would refuse it, while a scheme
//! handler sits outside the ACL. The page `fetch`es its own origin ([`route`] has the paths).
//! The handler runs on the event loop's thread, which a prompt must never block: every request
//! to the OS runs on a thread of its own and adds its outcome to the log when it returns, and
//! the page polls the log.
//!
//! The probe keeps nothing of the app's: Tauri's directories are keyed by an identifier of the
//! probe's own, and the webview's storage is a temporary directory (Linux, Windows) or a store
//! that keeps nothing (macOS, whose WKWebView takes no data directory). The directory is removed
//! on exit; on Windows WebView2 may still hold it then, and the probe names it on stderr for
//! removal by hand. No single-instance lock, so it runs beside the app. The password is moved
//! into `Zeroizing` as soon as it arrives and is never logged, echoed or kept.
//!
//! The session watch starts as the app's does, on the main thread before the event loop runs,
//! and the log says what it hears once it is set up.

use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use apprafter_core::CancellationToken;
use apprafter_os_auth::{Action, OsAuthenticator};
use serde::Serialize;
use tauri::http::{header, HeaderValue, Method, Response, StatusCode};
use tauri::{Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use zeroize::Zeroizing;

/// The page's own URI scheme.
const SCHEME: &str = "probe";
/// The window's label and title.
const LABEL: &str = "probe";
const TITLE: &str = "AppRafter OS auth probe";
/// Keys Tauri's directories apart from the app's (`dev.apprafter.desktop`).
const IDENTIFIER: &str = "dev.apprafter.desktop.os-auth-probe";
/// How long **Cancel in 3 s** lets the prompt stay open.
const CANCEL_AFTER: Duration = Duration::from_secs(3);
/// How long the session watch may take to listen before the log says it did not.
const WATCH_READY_WITHIN: Duration = Duration::from_secs(5);
/// The page's inline style and script, and requests to its own origin only.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                   connect-src 'self' probe: http://probe.localhost; base-uri 'none'; \
                   form-action 'none'";

/// The page: buttons that `POST` to the probe, and the log it polls. `__OS__` is
/// `std::env::consts::OS` ([`page`]), which shows the controls only one OS has.
const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="color-scheme" content="light dark">
<title>AppRafter OS auth probe</title>
<style>
  body { font: 14px system-ui, sans-serif; margin: 16px; }
  button { margin: 0 4px 6px 0; }
  .linux, .windows { display: none; }
  body[data-os="linux"] .linux, body[data-os="windows"] .windows { display: block; }
  #log { font: 12px ui-monospace, monospace; list-style: none; height: 65vh; overflow: auto;
         margin: 8px 0 0; padding: 8px; border: 1px solid #8888; white-space: pre-wrap; }
</style>
</head>
<body data-os="__OS__">
<div>
  <button data-post="/verify?action=unlock">Unlock</button>
  <button data-post="/verify?action=confirm">Confirm</button>
  <button data-post="/cancel-in-3s">Cancel in 3 s</button>
  <button data-post="/info">Read AuthInfo</button>
</div>
<div class="linux">
  <input id="password" type="password" autocomplete="off" placeholder="Password (PAM path)">
  <button data-password="unlock">Unlock with password</button>
  <button data-password="confirm">Confirm with password</button>
  <button data-post="/forget-agent">Forget the missing agent</button>
  <br><small>PAM checks the password only where polkit cannot prompt (no policy installed, a
  rules.d grant, no agent once a dialog found none); elsewhere the answer is
  Unavailable { NotPermittedHere }.</small>
</div>
<label class="windows"><input id="hello" type="checkbox" checked> Prefer Windows Hello</label>
<p id="status"></p>
<ul id="log"></ul>
<script>
  "use strict";
  const log = document.getElementById("log");
  const status = document.getElementById("status");
  const post = (path, body) =>
    fetch(path, { method: "POST", body }).catch((error) => {
      status.textContent = "The probe did not answer: " + error;
    });
  for (const button of document.querySelectorAll("[data-post]")) {
    button.onclick = () => post(button.dataset.post);
  }
  const password = document.getElementById("password");
  for (const button of document.querySelectorAll("[data-password]")) {
    button.onclick = () => {
      const value = password.value;
      password.value = "";
      post("/password?action=" + button.dataset.password, value);
    };
  }
  const hello = document.getElementById("hello");
  hello.onchange = () => post("/hello?on=" + hello.checked);
  let seen = 0;
  async function poll() {
    try {
      const entries = await (await fetch("/log?since=" + seen)).json();
      for (const entry of entries) {
        const item = document.createElement("li");
        item.textContent = entry.text;
        log.append(item);
        seen = entry.seq;
      }
      if (entries.length > 0) log.scrollTop = log.scrollHeight;
      status.textContent = "";
    } catch (error) {
      status.textContent = "The probe did not answer: " + error;
    }
    setTimeout(poll, 250);
  }
  poll();
</script>
</body>
</html>
"#;

/// What the page asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// `GET /`.
    Page,
    /// `GET /log?since=N`: the entries after the first `since`.
    Log { since: usize },
    /// `POST /verify?action=unlock|confirm`.
    Verify(Action),
    /// `POST /cancel-in-3s`: an Unlock whose token trips after [`CANCEL_AFTER`].
    CancelIn3s,
    /// `POST /password?action=unlock|confirm`, the password as the body: the PAM path (Linux).
    Password(Action),
    /// `POST /hello?on=true|false`: the `hello` setting (Windows).
    Hello(bool),
    /// `POST /info`: `AuthInfo` again.
    Info,
    /// `POST /forget-agent`: forget that the last polkit dialog found no agent (Linux).
    ForgetAgent,
    /// Anything else, a missing or unknown parameter included.
    NotFound,
}

/// The route of a request.
fn route(method: &Method, path: &str, query: Option<&str>) -> Route {
    let param = |key: &str| {
        query?
            .split('&')
            .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
    };
    let action = || {
        [Action::Unlock, Action::Confirm]
            .into_iter()
            .find(|&action| param("action") == Some(name(action)))
    };
    if *method == Method::GET {
        return match path {
            "/" => Route::Page,
            "/log" => Route::Log {
                since: param("since").and_then(|n| n.parse().ok()).unwrap_or(0),
            },
            _ => Route::NotFound,
        };
    }
    if *method != Method::POST {
        return Route::NotFound;
    }
    match path {
        "/verify" => action().map_or(Route::NotFound, Route::Verify),
        "/cancel-in-3s" => Route::CancelIn3s,
        "/password" => action().map_or(Route::NotFound, Route::Password),
        "/hello" => match param("on") {
            Some("true") => Route::Hello(true),
            Some("false") => Route::Hello(false),
            _ => Route::NotFound,
        },
        "/info" => Route::Info,
        "/forget-agent" => Route::ForgetAgent,
        _ => Route::NotFound,
    }
}

/// An action as the page names it.
const fn name(action: Action) -> &'static str {
    match action {
        Action::Unlock => "unlock",
        Action::Confirm => "confirm",
    }
}

/// The page's address. wry serves a custom scheme as `<scheme>://localhost/` on Linux and
/// macOS, and on Windows, where WebView2 has no custom schemes, as `http://<scheme>.localhost/`
/// (`Builder::register_asynchronous_uri_scheme_protocol`'s docs).
fn page_url(windows: bool) -> &'static str {
    if windows {
        "http://probe.localhost/"
    } else {
        "probe://localhost/"
    }
}

/// The page for `os` (`std::env::consts::OS`).
fn page(os: &str) -> String {
    PAGE.replace("__OS__", os)
}

/// One line of the log, numbered from 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Entry {
    seq: usize,
    text: String,
}

/// The entries after the first `since`: none when the page has them all.
fn after(entries: &[Entry], since: usize) -> &[Entry] {
    entries.get(since..).unwrap_or_default()
}

/// When an entry was made: the time of day in UTC, to match a screenshot, and the monotonic
/// time since the probe started, to time a prompt (`12:34:56.789Z +3.012s`).
fn stamp(elapsed: Duration, since_epoch: Duration) -> String {
    let day = since_epoch.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}Z +{}.{:03}s",
        day / 3_600,
        day / 60 % 60,
        day % 60,
        since_epoch.subsec_millis(),
        elapsed.as_secs(),
        elapsed.subsec_millis()
    )
}

/// A duration as the log gives it.
fn seconds(duration: Duration) -> String {
    format!("{:.3} s", duration.as_secs_f64())
}

/// What the page lists: kept in memory, and each entry also written to stderr.
struct Log {
    /// The probe's start: the log's monotonic clock and PAM's.
    started: Instant,
    entries: Mutex<Vec<Entry>>,
    /// Where an entry is written out once it is in: stderr.
    write: fn(&Log, &str),
}

impl Log {
    fn new() -> Self {
        Self::writing_with(|_, text| eprintln!("{text}"))
    }

    fn writing_with(write: fn(&Log, &str)) -> Self {
        Self {
            started: Instant::now(),
            entries: Mutex::default(),
            write,
        }
    }

    /// Adds an entry, then writes it out with the lock released: the page's `/log` handler takes
    /// that lock on the event loop's thread, which must not wait for stderr. Two threads' lines
    /// may reach stderr in another order than the page shows; the stamp orders them.
    fn push(&self, text: impl AsRef<str>) {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let text = format!(
            "{} {}",
            stamp(self.started.elapsed(), since_epoch),
            text.as_ref()
        );
        {
            let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            let seq = entries.len() + 1;
            entries.push(Entry {
                seq,
                text: text.clone(),
            });
        }
        (self.write)(self, &text);
    }

    fn after(&self, since: usize) -> Vec<Entry> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        after(&entries, since).to_vec()
    }

    /// Milliseconds since the probe started, the monotonic clock PAM's back-off reads.
    #[cfg(target_os = "linux")]
    fn monotonic_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The probe's state, shared by the scheme handler and the request threads.
struct Probe {
    auth: OsAuthenticator,
    log: Log,
    /// The last request's number.
    requests: AtomicU64,
}

impl Probe {
    fn next_request(&self) -> u64 {
        self.requests.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Runs `work` on a thread of its own: everything that asks the OS blocks.
    fn spawn(self: &Arc<Self>, work: impl FnOnce(&Self) + Send + 'static) {
        let probe = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("probe-request".to_owned())
            .spawn(move || work(&probe));
        if let Err(error) = spawned {
            self.log.push(format!("no thread for the request: {error}"));
        }
    }

    /// Logs `AuthInfo`. Blocks: polkitd on Linux, Hello's availability on Windows.
    fn log_info(&self) {
        let info = self.auth.info();
        self.log.push(format!("AuthInfo: {info:?}"));
    }

    fn handle(self: &Arc<Self>, route: Route, body: Vec<u8>) -> Response<Vec<u8>> {
        match route {
            Route::Page => respond(
                StatusCode::OK,
                "text/html; charset=utf-8",
                page(std::env::consts::OS).into_bytes(),
            ),
            Route::Log { since } => respond(
                StatusCode::OK,
                "application/json",
                serde_json::to_vec(&self.log.after(since)).unwrap_or_else(|_| b"[]".to_vec()),
            ),
            Route::Verify(action) => {
                self.ask(action, None);
                accepted()
            }
            Route::CancelIn3s => {
                self.ask(Action::Unlock, Some(CANCEL_AFTER));
                accepted()
            }
            Route::Password(action) => self.password(action, body),
            Route::Hello(on) => self.hello(on),
            Route::Info => {
                self.spawn(Self::log_info);
                accepted()
            }
            Route::ForgetAgent => self.forget_agent(),
            Route::NotFound => respond(StatusCode::NOT_FOUND, "text/plain", Vec::new()),
        }
    }

    /// Asks the OS to authenticate for `action`; with `cancel_after`, trips the request's token
    /// once that long has passed without an answer.
    fn ask(self: &Arc<Self>, action: Action, cancel_after: Option<Duration>) {
        let id = self.next_request();
        let what = match cancel_after {
            None => name(action).to_owned(),
            Some(after) => format!("{} (cancel in {} s)", name(action), after.as_secs()),
        };
        let token = CancellationToken::new();
        // Dropped when the OS answers, which tells the canceller it has nothing to do.
        let (answered, answer) = mpsc::channel::<()>();
        if let Some(after) = cancel_after {
            let token = token.clone();
            self.spawn(move |probe| {
                if answer.recv_timeout(after) == Err(RecvTimeoutError::Timeout) {
                    probe.log.push(format!("#{id}: cancelling its token"));
                    token.cancel();
                }
            });
        }
        self.spawn(move |probe| {
            probe.log.push(format!("#{id} {what}: asking the OS"));
            let started = Instant::now();
            let outcome = probe.auth.verify(action, &token);
            drop(answered);
            let took = seconds(started.elapsed());
            probe
                .log
                .push(format!("#{id} {what}: {outcome:?} after {took}"));
            probe.log_info();
        });
    }

    /// The PAM path: checks the password from the page's field (Linux).
    fn password(self: &Arc<Self>, action: Action, body: Vec<u8>) -> Response<Vec<u8>> {
        // Into `Zeroizing` before anything reads it; never logged, echoed or kept.
        let password = match String::from_utf8(body) {
            Ok(password) => Zeroizing::new(password),
            Err(error) => {
                drop(Zeroizing::new(error.into_bytes()));
                self.log.push("the password is not UTF-8: not checked");
                return respond(StatusCode::BAD_REQUEST, "text/plain", Vec::new());
            }
        };
        #[cfg(target_os = "linux")]
        {
            let id = self.next_request();
            let what = format!("{} with the password", name(action));
            self.spawn(move |probe| {
                probe.log.push(format!("#{id} {what}: checking"));
                let started = Instant::now();
                let check = probe.auth.verify_password(
                    action,
                    password,
                    &CancellationToken::new(),
                    probe.log.monotonic_ms(),
                );
                let took = seconds(started.elapsed());
                for message in &check.messages {
                    probe.log.push(format!("#{id} PAM said: {message}"));
                }
                probe
                    .log
                    .push(format!("#{id} {what}: {:?} after {took}", check.outcome));
                probe.log_info();
            });
            accepted()
        }
        #[cfg(not(target_os = "linux"))]
        {
            drop(password);
            self.log.push(format!(
                "{} with the password: the PAM path is Linux only",
                name(action)
            ));
            respond(StatusCode::NOT_FOUND, "text/plain", Vec::new())
        }
    }

    /// The `hello` setting (Windows).
    fn hello(self: &Arc<Self>, on: bool) -> Response<Vec<u8>> {
        #[cfg(windows)]
        {
            self.auth.set_hello(on);
            self.log.push(format!("prefer Windows Hello: {on}"));
            self.spawn(Self::log_info);
            accepted()
        }
        #[cfg(not(windows))]
        {
            self.log
                .push(format!("prefer Windows Hello ({on}): Windows only"));
            respond(StatusCode::NOT_FOUND, "text/plain", Vec::new())
        }
    }

    /// Forgets a missing polkit agent, as the app does on each lock-screen entry (Linux).
    fn forget_agent(self: &Arc<Self>) -> Response<Vec<u8>> {
        #[cfg(target_os = "linux")]
        {
            self.auth.reset_agent_memory();
            self.log.push("forgot the missing agent");
            self.spawn(Self::log_info);
            accepted()
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.log.push("forgetting the missing agent: Linux only");
            respond(StatusCode::NOT_FOUND, "text/plain", Vec::new())
        }
    }
}

fn respond(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response<Vec<u8>> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    response
}

/// A request started; its outcome arrives in the log.
fn accepted() -> Response<Vec<u8>> {
    respond(StatusCode::ACCEPTED, "text/plain", Vec::new())
}

/// Opens the probe's window on its page, with webview storage of its own (the module docs).
fn open_window(app: &tauri::App, probe: &Probe, webview_data: PathBuf) -> tauri::Result<()> {
    let url = Url::parse(page_url(cfg!(windows))).map_err(tauri::Error::InvalidUrl)?;
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::CustomProtocol(url))
        .title(TITLE)
        .inner_size(860.0, 720.0);
    #[cfg(not(target_os = "macos"))]
    let window = window.data_directory(webview_data);
    // WKWebView takes no data directory, and a data store identifier only from macOS 14.
    #[cfg(target_os = "macos")]
    let window = {
        drop(webview_data);
        window.incognito(true)
    };
    parent_prompts(probe, &window.build()?)
}

/// Windows: Hello and the credential dialog are parented to the window, and without one a
/// request opens nothing (`Unavailable { NotInteractive }`).
#[cfg(windows)]
fn parent_prompts(probe: &Probe, window: &WebviewWindow) -> tauri::Result<()> {
    let hwnd = window.hwnd()?.0 as isize;
    probe.auth.set_window(hwnd);
    probe.log.push(format!(
        "prompts are parented to the window (HWND {hwnd:#x})"
    ));
    Ok(())
}

#[cfg(not(windows))]
fn parent_prompts(_: &Probe, _: &WebviewWindow) -> tauri::Result<()> {
    Ok(())
}

/// Watches the OS's lock and sleep signals as the app does: the watch starts here, on the main
/// thread before the event loop runs (macOS delivers through that loop), and a thread of its
/// own waits for it to say what it hears, then keeps it until the returned sender is dropped.
/// The watch's callback takes the log's lock, which that thread never holds while it drops the
/// watch.
fn watch_session(probe: &Arc<Probe>) -> Option<(mpsc::Sender<()>, JoinHandle<()>)> {
    let watch = apprafter_os_auth::watch({
        let probe = Arc::clone(probe);
        move |event| probe.log.push(format!("session: {event:?}"))
    });
    let (stop, stopped) = mpsc::channel::<()>();
    let spawned = thread::Builder::new()
        .name("probe-session-watch".to_owned())
        .spawn({
            let probe = Arc::clone(probe);
            move || {
                match watch.listening(WATCH_READY_WITHIN) {
                    Some(listening) => probe
                        .log
                        .push(format!("session watch: ready, {listening:?}")),
                    None => probe.log.push(format!(
                        "session watch: not ready within {}",
                        seconds(WATCH_READY_WITHIN)
                    )),
                }
                // Until the probe exits.
                let _ = stopped.recv();
                drop(watch);
            }
        });
    match spawned {
        Ok(watching) => Some((stop, watching)),
        Err(error) => {
            // The watch went with the closure: nothing is watched.
            probe
                .log
                .push(format!("no thread for the session watch: {error}"));
            None
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // The webview's storage on Linux and Windows, removed on exit when the webview has let go of
    // it (macOS: `open_window`).
    let webview_data = tempfile::Builder::new()
        .prefix("apprafter-os-auth-probe-")
        .tempdir()?;
    let probe = Arc::new(Probe {
        auth: OsAuthenticator::new(),
        log: Log::new(),
        requests: AtomicU64::new(0),
    });
    probe.log.push(format!(
        "{TITLE} on {} {}, page {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        page_url(cfg!(windows))
    ));
    probe.spawn(Probe::log_info);

    let mut context = tauri::generate_context!();
    context.config_mut().identifier = IDENTIFIER.to_owned();
    let app = tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol(SCHEME, {
            let probe = Arc::clone(&probe);
            move |_, request, responder| {
                let uri = request.uri();
                let route = route(request.method(), uri.path(), uri.query());
                responder.respond(probe.handle(route, request.into_body()));
            }
        })
        .setup({
            let probe = Arc::clone(&probe);
            let webview_data = webview_data.path().to_owned();
            move |app| Ok(open_window(app, &probe, webview_data)?)
        })
        .build(context)?;
    let watch = watch_session(&probe);
    let code = app.run_return(|_, _| {});
    if let Some((stop, watching)) = watch {
        drop(stop);
        let _ = watching.join();
    }
    let storage = webview_data.path().to_owned();
    if let Err(error) = webview_data.close() {
        // WebView2 can hold its files a moment past the window.
        eprintln!(
            "the webview's storage {} was not removed ({error}); remove it by hand",
            storage.display()
        );
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(path: &str, query: Option<&str>) -> Route {
        route(&Method::GET, path, query)
    }

    fn post(path: &str, query: Option<&str>) -> Route {
        route(&Method::POST, path, query)
    }

    #[test]
    fn the_page_and_the_log_are_read_with_get() {
        assert_eq!(get("/", None), Route::Page);
        assert_eq!(get("/log", None), Route::Log { since: 0 });
        assert_eq!(get("/log", Some("since=7")), Route::Log { since: 7 });
        assert_eq!(get("/log", Some("since=")), Route::Log { since: 0 });
        assert_eq!(get("/log", Some("since=x")), Route::Log { since: 0 });
        assert_eq!(get("/favicon.ico", None), Route::NotFound);
    }

    #[test]
    fn every_action_is_posted_by_its_name() {
        for action in [Action::Unlock, Action::Confirm] {
            let query = format!("action={}", name(action));
            assert_eq!(
                post("/verify", Some(&query)),
                Route::Verify(action),
                "{action:?}"
            );
            assert_eq!(
                post("/password", Some(&query)),
                Route::Password(action),
                "{action:?}"
            );
        }
        assert_eq!(name(Action::Unlock), "unlock");
        assert_eq!(name(Action::Confirm), "confirm");
    }

    #[test]
    fn the_other_requests_are_posted() {
        assert_eq!(post("/cancel-in-3s", None), Route::CancelIn3s);
        assert_eq!(post("/info", None), Route::Info);
        assert_eq!(post("/forget-agent", None), Route::ForgetAgent);
        assert_eq!(post("/hello", Some("on=true")), Route::Hello(true));
        assert_eq!(post("/hello", Some("on=false")), Route::Hello(false));
    }

    /// Nothing that would ask the OS runs from a request the page does not make.
    #[test]
    fn a_missing_or_unknown_parameter_or_method_asks_nothing() {
        for (method, path, query) in [
            (Method::POST, "/verify", None),
            (Method::POST, "/verify", Some("action=")),
            (Method::POST, "/verify", Some("action=delete")),
            (Method::POST, "/verify", Some("actions=unlock")),
            (Method::POST, "/verify", Some("xaction=unlock")),
            (Method::POST, "/password", Some("action=Unlock")),
            (Method::POST, "/hello", None),
            (Method::POST, "/hello", Some("on=1")),
            (Method::POST, "/hello", Some("once=true")),
            (Method::GET, "/verify", Some("action=unlock")),
            (Method::GET, "/cancel-in-3s", None),
            (Method::GET, "/info", None),
            (Method::POST, "/", None),
            (Method::POST, "/log", None),
            (Method::PUT, "/info", None),
            (Method::POST, "/verify/", Some("action=unlock")),
        ] {
            assert_eq!(
                route(&method, path, query),
                Route::NotFound,
                "{method} {path} {query:?}"
            );
        }
    }

    #[test]
    fn a_parameter_is_found_among_others() {
        assert_eq!(
            post("/verify", Some("x=1&action=confirm&y")),
            Route::Verify(Action::Confirm)
        );
        assert_eq!(get("/log", Some("t=5&since=12")), Route::Log { since: 12 });
    }

    #[test]
    fn the_page_is_where_wry_serves_the_scheme() {
        assert_eq!(page_url(false), "probe://localhost/");
        assert_eq!(page_url(true), "http://probe.localhost/");
        for windows in [false, true] {
            let url = Url::parse(page_url(windows)).unwrap();
            assert_eq!(url.path(), "/", "{url}");
            assert!(url.as_str().contains(SCHEME), "{url}");
        }
    }

    #[test]
    fn the_page_shows_the_controls_of_its_os() {
        let linux = page("linux");
        assert!(linux.contains(r#"<body data-os="linux">"#));
        assert!(!linux.contains("__OS__"));
        assert!(page("windows").contains(r#"<body data-os="windows">"#));
        assert!(linux.contains("<title>AppRafter OS auth probe</title>"));
    }

    fn entries(count: usize) -> Vec<Entry> {
        (1..=count)
            .map(|seq| Entry {
                seq,
                text: format!("entry {seq}"),
            })
            .collect()
    }

    #[test]
    fn the_log_gives_the_entries_the_page_has_not_seen() {
        let all = entries(3);
        assert_eq!(after(&all, 0), &all[..]);
        assert_eq!(after(&all, 1), &all[1..]);
        assert_eq!(after(&all, 3), &[] as &[Entry]);
        assert_eq!(after(&all, 9), &[] as &[Entry], "a page ahead of the log");
        assert_eq!(after(&[], 0), &[] as &[Entry]);
    }

    #[test]
    fn an_entry_is_stamped_with_utc_and_the_time_since_start() {
        // 1_700_000_000 s after the epoch is 22:13:20 UTC.
        let epoch = Duration::from_secs(1_700_000_000) + Duration::from_millis(789);
        assert_eq!(
            stamp(Duration::from_millis(3_012), epoch),
            "22:13:20.789Z +3.012s"
        );
        assert_eq!(
            stamp(Duration::ZERO, Duration::from_millis(86_400_005)),
            "00:00:00.005Z +0.000s"
        );
        assert_eq!(
            stamp(Duration::from_secs(4_000), Duration::from_secs(86_399)),
            "23:59:59.000Z +4000.000s"
        );
    }

    /// The writer runs with the log's lock free: the page's `/log` reads, on the event loop,
    /// never wait for stderr.
    #[test]
    fn an_entry_is_written_out_with_the_log_s_lock_free() {
        fn write(log: &Log, text: &str) {
            assert!(
                log.entries.try_lock().is_ok(),
                "written under the log's lock: {text}"
            );
        }
        let log = Log::writing_with(write);
        log.push("first");
        log.push("second");
        assert_eq!(log.after(0).len(), 2);
    }

    #[test]
    fn the_log_numbers_its_entries_from_one() {
        let log = Log::new();
        log.push("first");
        log.push("second");
        let all = log.after(0);
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);
        assert!(all[0].text.ends_with(" first"), "{}", all[0].text);
        assert_eq!(log.after(1), all[1..]);
    }
}
