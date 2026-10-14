// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What this computer lets other programs running as the person do (ADR 0068 §7, Risks; D.4a
//! T72, O1, G13): the facts behind `lock status`'s "This computer" lines, doctor's machine-limit
//! rows (d4a-f) and Settings › Lock (d4a-g). The core reads files — Yama's `ptrace_scope`, the SSH
//! key's header and the `.pub` beside it — and asks an ssh-agent at the socket the client names;
//! it reads no environment. What only a client can see comes in [`LimitClient`] (T73).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::context::Context;
use crate::lock::{HolderBaseSource, MachineLimit, UnlockWindow};
use crate::Zeroizing;

/// How long the ssh-agent may take to list its keys (G13).
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(2);
/// The most of a key file read: a 16 KiB private key is far larger than any OpenSSH writes.
const KEY_READ_MAX: usize = 16 << 10;

/// What only the client can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitClient {
    /// `terminal` is `None` when the command was not run from one.
    Cli {
        terminal: Option<TerminalFacts>,
        ssh: SshFacts,
    },
    Desktop {
        session_events: SessionEvents,
        display: DisplayKind,
        ssh: SshFacts,
    },
}

/// The CLI's terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFacts {
    /// `tmux`, `screen`, `zellij`, `wezterm` or `kitty`, when the terminal runs inside one.
    pub multiplexer: Option<String>,
    /// The terminal is an X client (`WINDOWID` set): a heuristic, UNVERIFIED per terminal.
    pub x_client: Option<bool>,
    /// The session's display (`XDG_SESSION_TYPE`, `WAYLAND_DISPLAY`, `DISPLAY`).
    pub display: DisplayKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayKind {
    X11,
    Wayland,
    Xwayland,
    None,
}

/// What the OS tells the desktop app (its `AppInfo.sessionEvents`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEvents {
    pub lock: bool,
    pub sleep: bool,
}

/// The SSH key that fetches kubeconfigs, and the ssh-agent to ask about it (O1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshFacts {
    pub key: Option<PathBuf>,
    pub agent: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAgentAnswer {
    Listed,
    NotListed,
    /// The agent did not answer within the timeout, or its answer was no listing (G13).
    Unknown,
}

/// What a private key's file says without its passphrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFacts {
    pub unencrypted: bool,
    /// The public key's wire blob, when the file (or the `.pub` beside it) carries it.
    pub public_blob: Option<Vec<u8>>,
}

/// This computer's limits, in §3.13's order (`lock status` shows its warnings first, each kind
/// in this order).
pub fn detect_limits(
    ctx: &Context,
    client: &LimitClient,
    window: &UnlockWindow,
) -> Vec<MachineLimit> {
    let mut out = Vec::new();
    let (display, ssh) = match client {
        LimitClient::Cli { terminal, ssh } => {
            (terminal.as_ref().map(|t| (t.display, t.x_client)), ssh)
        }
        LimitClient::Desktop { display, ssh, .. } => (Some((*display, None)), ssh),
    };
    #[cfg(target_os = "linux")]
    {
        match display {
            Some((DisplayKind::X11, _)) => out.push(MachineLimit::X11Session),
            Some((DisplayKind::Xwayland, x)) => out.push(MachineLimit::Xwayland {
                terminal_is_x_client: x,
            }),
            _ => {}
        }
        if let Some(value) = ptrace_scope_limit(std::fs::read_to_string(
            "/proc/sys/kernel/yama/ptrace_scope",
        )) {
            out.push(MachineLimit::PtraceScope { value });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = display;
    #[cfg(windows)]
    out.push(MachineLimit::WindowsSameUser);
    #[cfg(target_os = "macos")]
    out.push(MachineLimit::MacosTaskPort);
    if let LimitClient::Cli {
        terminal: Some(t), ..
    } = client
    {
        if let Some(name) = &t.multiplexer {
            out.push(MachineLimit::Multiplexer { name: name.clone() });
        }
        if matches!(window, UnlockWindow::Open { .. }) {
            out.push(MachineLimit::InheritedWindow);
        }
    }
    if let LimitClient::Desktop { session_events, .. } = client {
        if !session_events.lock && !session_events.sleep {
            out.push(MachineLimit::SleepUnreported);
        }
    }
    if let Some(row) = ssh_limit(ctx, ssh) {
        out.push(row);
    }
    if let Some(base) = ctx.holder_base() {
        if base.source != HolderBaseSource::XdgRuntimeDir {
            out.push(MachineLimit::RuntimeDirFallback {
                path: base.dir.clone(),
            });
        }
    }
    out.push(MachineLimit::SecretsInUse);
    out.push(MachineLimit::SameUserPrograms);
    out.push(MachineLimit::Hibernation);
    out
}

/// T72: Yama absent (`NotFound`) or scope 0 is a limit; 1 to 3 are not; anything else read is
/// no fact to state. (Linux reads it; its test runs on every OS.)
#[cfg(any(target_os = "linux", test))]
pub(crate) fn ptrace_scope_limit(read: io::Result<String>) -> Option<Option<u8>> {
    match read {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
        Ok(s) => (s.trim().parse::<u8>().ok() == Some(0)).then_some(Some(0)),
    }
}

/// O1: the fetch key is a limit when it has no passphrase or an agent holds it.
fn ssh_limit(ctx: &Context, ssh: &SshFacts) -> Option<MachineLimit> {
    let key = ssh.key.as_ref()?;
    let facts = ssh_key_facts(key).ok()??;
    let in_agent = match (&ssh.agent, &facts.public_blob) {
        (None, _) => Some(false),
        (Some(sock), Some(blob)) => match query_agent(sock, blob, AGENT_TIMEOUT) {
            SshAgentAnswer::Listed => Some(true),
            SshAgentAnswer::NotListed => Some(false),
            SshAgentAnswer::Unknown => None,
        },
        (Some(_), None) => None,
    };
    (facts.unencrypted || in_agent != Some(false)).then(|| MachineLimit::SshKeyUsable {
        path: cli_core::paths::abbreviate_home(key, ctx.home_dir()),
        unencrypted: facts.unencrypted,
        in_agent,
    })
}

/// What the key file at `path` says (`Ok(None)`: no such file, or no private key it knows).
/// The file is read into a bounded buffer zeroed on drop: an unencrypted key is the secret.
pub fn ssh_key_facts(path: &Path) -> io::Result<Option<KeyFacts>> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut buf = Zeroizing::new(vec![0u8; KEY_READ_MAX]);
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            m => n += m,
        }
    }
    let Ok(text) = std::str::from_utf8(&buf[..n]) else {
        return Ok(None);
    };
    let Some(mut facts) = key_facts_of(text) else {
        return Ok(None);
    };
    if facts.public_blob.is_none() {
        let mut pub_path = path.as_os_str().to_owned();
        pub_path.push(".pub");
        facts.public_blob = std::fs::read_to_string(PathBuf::from(pub_path))
            .ok()
            .and_then(|line| line.split_whitespace().nth(1).and_then(b64_decode));
    }
    Ok(Some(facts))
}

/// A private key's facts from its text (the formats OpenSSH reads).
pub(crate) fn key_facts_of(text: &str) -> Option<KeyFacts> {
    let text = text.trim_start();
    const OPENSSH: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
    if let Some(rest) = text.strip_prefix(OPENSSH) {
        let body = rest.split("-----END").next()?;
        let joined: Zeroizing<String> =
            Zeroizing::new(body.chars().filter(|c| !c.is_whitespace()).collect());
        let bytes = Zeroizing::new(b64_decode(&joined)?);
        return openssh_facts(&bytes);
    }
    for label in ["RSA", "EC", "DSA"] {
        if text.starts_with(&format!("-----BEGIN {label} PRIVATE KEY-----")) {
            return Some(KeyFacts {
                unencrypted: !text.contains("Proc-Type: 4,ENCRYPTED"),
                public_blob: None,
            });
        }
    }
    if text.starts_with("-----BEGIN PRIVATE KEY-----") {
        return Some(KeyFacts {
            unencrypted: true,
            public_blob: None,
        });
    }
    if text.starts_with("-----BEGIN ENCRYPTED PRIVATE KEY-----") {
        return Some(KeyFacts {
            unencrypted: false,
            public_blob: None,
        });
    }
    None
}

/// `openssh-key-v1`: magic, cipher name, KDF name, KDF options, key count, then the public keys
/// in the clear (PROTOCOL.key).
fn openssh_facts(b: &[u8]) -> Option<KeyFacts> {
    let rest = b.strip_prefix(b"openssh-key-v1\0")?;
    let (cipher, rest) = ssh_string(rest)?;
    let (_kdf, rest) = ssh_string(rest)?;
    let (_options, rest) = ssh_string(rest)?;
    let count = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
    if count < 1 {
        return None;
    }
    let (public, _) = ssh_string(rest.get(4..)?)?;
    Some(KeyFacts {
        unencrypted: cipher == b"none",
        public_blob: Some(public.to_vec()),
    })
}

/// One SSH wire string: a big-endian u32 length, then that many bytes.
fn ssh_string(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let len = u32::from_be_bytes(b.get(..4)?.try_into().ok()?) as usize;
    let end = 4usize.checked_add(len)?;
    Some((b.get(4..end)?, b.get(end..)?))
}

/// Standard base64 (RFC 4648 §4), padding optional; `None` on any other character.
pub(crate) fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Whether the ssh-agent at `sock` lists the public key `blob`.
pub fn query_agent(sock: &Path, blob: &[u8], timeout: Duration) -> SshAgentAnswer {
    match agent_identities(sock, timeout) {
        Ok(keys) if keys.iter().any(|k| k == blob) => SshAgentAnswer::Listed,
        Ok(_) => SshAgentAnswer::NotListed,
        Err(_) => SshAgentAnswer::Unknown,
    }
}

#[cfg(unix)]
fn agent_identities(sock: &Path, timeout: Duration) -> io::Result<Vec<Vec<u8>>> {
    let mut s = std::os::unix::net::UnixStream::connect(sock)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    exchange(&mut s)
}

/// A named pipe opened as a file has no per-read timeout: the exchange runs on a thread, given up
/// past `timeout`.
#[cfg(windows)]
fn agent_identities(sock: &Path, timeout: Duration) -> io::Result<Vec<Vec<u8>>> {
    let path = sock.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .and_then(|mut f| exchange(&mut f));
        let _ = tx.send(r);
    });
    rx.recv_timeout(timeout)
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
}

/// `SSH2_AGENTC_REQUEST_IDENTITIES` (11), then its answer (12): the keys' blobs.
fn exchange(s: &mut (impl Read + Write)) -> io::Result<Vec<Vec<u8>>> {
    s.write_all(&[0, 0, 0, 1, 11])?;
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > 256 << 10 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    parse_identities(&body).ok_or_else(|| io::ErrorKind::InvalidData.into())
}

fn parse_identities(body: &[u8]) -> Option<Vec<Vec<u8>>> {
    let (&code, rest) = body.split_first()?;
    if code != 12 {
        return None;
    }
    let n = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
    let mut rest = rest.get(4..)?;
    let mut keys = Vec::new();
    for _ in 0..n {
        let (blob, r) = ssh_string(rest)?;
        let (_comment, r) = ssh_string(r)?;
        keys.push(blob.to_vec());
        rest = r;
    }
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;
    use crate::lock::{HolderHardening, MachineLimit, UnlockWindow};

    const UNENCRYPTED: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACAREREREREREREREREREREREREREREREREREREREREREQAAABAAAAAAAAAA
AAAAAAAAAAAA
-----END OPENSSH PRIVATE KEY-----
";
    const ENCRYPTED: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABAiIiIiIi
IiIiIiIiIiIiIiAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIBERERERERERERER
ERERERERERERERERERERERERERERAAAAEAAAAAAAAAAAAAAAAAAAAAA=
-----END OPENSSH PRIVATE KEY-----
";

    fn s(b: &[u8]) -> Vec<u8> {
        let mut out = (b.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b);
        out
    }

    /// `ssh-ed25519` with a public key of 32 × 0x11: the fixtures' public half.
    fn public() -> Vec<u8> {
        [s(b"ssh-ed25519"), s(&[0x11; 32])].concat()
    }

    #[test]
    fn base64_decodes_rfc_4648s_vectors() {
        for (enc, dec) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg==", "foob"),
            ("Zm9vYmFy", "foobar"),
        ] {
            assert_eq!(b64_decode(enc).unwrap(), dec.as_bytes(), "{enc}");
        }
        assert_eq!(b64_decode("Zm9v!"), None);
    }

    /// O1: an OpenSSH key's cipher name says whether it has a passphrase; its public half is in
    /// the clear part of the file, for the agent query.
    #[test]
    fn an_openssh_key_says_whether_it_has_a_passphrase_and_its_public_half() {
        assert_eq!(
            key_facts_of(UNENCRYPTED),
            Some(KeyFacts {
                unencrypted: true,
                public_blob: Some(public())
            })
        );
        assert_eq!(
            key_facts_of(ENCRYPTED),
            Some(KeyFacts {
                unencrypted: false,
                public_blob: Some(public())
            })
        );
    }

    #[test]
    fn a_pem_key_says_it_through_its_header() {
        let rsa = "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\n";
        let rsa_enc = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00\n\nMIIB\n-----END RSA PRIVATE KEY-----\n";
        assert_eq!(key_facts_of(rsa).map(|f| f.unencrypted), Some(true));
        assert_eq!(key_facts_of(rsa_enc).map(|f| f.unencrypted), Some(false));
        assert_eq!(
            key_facts_of("-----BEGIN PRIVATE KEY-----\nMII\n").map(|f| f.unencrypted),
            Some(true)
        );
        assert_eq!(
            key_facts_of("-----BEGIN ENCRYPTED PRIVATE KEY-----\nMII\n").map(|f| f.unencrypted),
            Some(false)
        );
        assert_eq!(key_facts_of("ssh-ed25519 AAAA… a public key"), None);
    }

    /// A PEM key's public half comes from the `.pub` beside it; a missing key file is no limit.
    #[test]
    fn a_key_file_is_read_with_its_pub_and_a_missing_one_is_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id_rsa");
        std::fs::write(
            &key,
            "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\n",
        )
        .unwrap();
        let b64 = "AAAAC3NzaC1lZDI1NTE5AAAAIBERERERERERERERERERERERERERERERERERERERERER";
        std::fs::write(
            dir.path().join("id_rsa.pub"),
            format!("ssh-ed25519 {b64} me@host\n"),
        )
        .unwrap();
        assert_eq!(
            ssh_key_facts(&key).unwrap(),
            Some(KeyFacts {
                unencrypted: true,
                public_blob: Some(public())
            })
        );
        assert_eq!(ssh_key_facts(&dir.path().join("none")).unwrap(), None);
    }

    /// A fake ssh-agent on a socket of its own: answers `answer` to one request.
    #[cfg(unix)]
    fn agent(answer: Option<Vec<u8>>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let path = dir.path().join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut req = [0u8; 5];
            c.read_exact(&mut req).unwrap();
            assert_eq!(req, [0, 0, 0, 1, 11], "SSH2_AGENTC_REQUEST_IDENTITIES");
            match answer {
                Some(body) => {
                    c.write_all(&(body.len() as u32).to_be_bytes()).unwrap();
                    c.write_all(&body).unwrap();
                }
                None => std::thread::sleep(Duration::from_secs(5)),
            }
        });
        (dir, path)
    }

    /// G13: listed, not listed, and an agent that does not answer within the timeout: unknown.
    #[cfg(unix)]
    #[test]
    fn the_agent_is_asked_whether_it_holds_the_key() {
        let other = [s(b"ssh-ed25519"), s(&[0x22; 32])].concat();
        let listing = |keys: &[&[u8]]| {
            let mut body = vec![12u8];
            body.extend_from_slice(&(keys.len() as u32).to_be_bytes());
            for k in keys {
                body.extend(s(k));
                body.extend(s(b"comment"));
            }
            body
        };
        let (_d1, a1) = agent(Some(listing(&[&other, &public()])));
        assert_eq!(
            query_agent(&a1, &public(), Duration::from_secs(2)),
            SshAgentAnswer::Listed
        );
        let (_d2, a2) = agent(Some(listing(&[&other])));
        assert_eq!(
            query_agent(&a2, &public(), Duration::from_secs(2)),
            SshAgentAnswer::NotListed
        );
        let (_d3, a3) = agent(None);
        assert_eq!(
            query_agent(&a3, &public(), Duration::from_millis(200)),
            SshAgentAnswer::Unknown
        );
        assert_eq!(
            query_agent(
                &a3.with_file_name("gone.sock"),
                &public(),
                Duration::from_secs(2)
            ),
            SshAgentAnswer::Unknown
        );
    }

    #[test]
    fn ptrace_scope_zero_or_absent_is_a_limit_and_one_to_three_are_not() {
        let nf = || Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(ptrace_scope_limit(Ok("0\n".into())), Some(Some(0)));
        assert_eq!(ptrace_scope_limit(nf()), Some(None), "no Yama");
        for v in ["1\n", "2", "3"] {
            assert_eq!(ptrace_scope_limit(Ok(v.into())), None, "{v}");
        }
        assert_eq!(
            ptrace_scope_limit(Err(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            ))),
            None
        );
    }

    /// The CLI's context as far as these rows read it — the store root and the holder's base
    /// `from_cli_env` would take from `env` — built from nothing of the real environment (G3
    /// counts `from_cli_env` in a test as a real-environment builder; critic-d4a-e finding 1).
    fn ctx_with(env: MapEnv) -> Context {
        let root = Path::new("/tmp/s");
        let base =
            crate::context::holder_base_for(crate::context::HolderOs::CURRENT, &env, root, || None);
        Context::for_desktop(root.to_path_buf(), "http://127.0.0.1:1").with_holder_base(base)
    }

    fn cli(
        display: DisplayKind,
        multiplexer: Option<&str>,
        x: Option<bool>,
        ssh: SshFacts,
    ) -> LimitClient {
        LimitClient::Cli {
            terminal: Some(TerminalFacts {
                multiplexer: multiplexer.map(str::to_string),
                x_client: x,
                display,
            }),
            ssh,
        }
    }

    fn open() -> UnlockWindow {
        UnlockWindow::Open {
            left_secs: 60,
            minutes: 5,
            holder_version: "0.0.0".into(),
            hardening: HolderHardening {
                non_dumpable: Some(true),
                deny_attach: None,
                core_off: true,
                memory_locked: true,
                process_dacl: None,
            },
        }
    }

    /// T72: the static rows are always there; the window's row only with an open window (CLI);
    /// a multiplexer when the terminal runs inside one; the fallback base when the runtime dir
    /// is not the source. Each OS's own rows on that OS.
    #[test]
    fn the_rows_follow_the_facts() {
        let ctx = ctx_with(MapEnv::new().with("XDG_RUNTIME_DIR", "/run/x"));
        let got = detect_limits(
            &ctx,
            &cli(
                DisplayKind::Wayland,
                Some("tmux"),
                None,
                SshFacts::default(),
            ),
            &open(),
        );
        assert!(got.contains(&MachineLimit::Multiplexer {
            name: "tmux".into()
        }));
        assert!(got.contains(&MachineLimit::InheritedWindow));
        assert!(
            got.contains(&MachineLimit::Hibernation) && got.contains(&MachineLimit::SecretsInUse)
        );
        assert!(!got
            .iter()
            .any(|l| matches!(l, MachineLimit::RuntimeDirFallback { .. })));
        let closed = detect_limits(
            &ctx,
            &cli(DisplayKind::Wayland, None, None, SshFacts::default()),
            &UnlockWindow::Closed,
        );
        assert!(!closed.contains(&MachineLimit::InheritedWindow));
        let desk = LimitClient::Desktop {
            session_events: SessionEvents {
                lock: false,
                sleep: false,
            },
            display: DisplayKind::Wayland,
            ssh: SshFacts::default(),
        };
        let d = detect_limits(&ctx, &desk, &UnlockWindow::App);
        assert!(
            d.contains(&MachineLimit::SleepUnreported)
                && !d.contains(&MachineLimit::InheritedWindow)
        );
        #[cfg(windows)]
        assert!(got.contains(&MachineLimit::WindowsSameUser));
        #[cfg(target_os = "macos")]
        assert!(got.contains(&MachineLimit::MacosTaskPort));
    }

    /// T72 (Linux): an X11 session is a warning; Xwayland names whether the terminal is an X client.
    #[cfg(target_os = "linux")]
    #[test]
    fn x11_and_xwayland_are_rows_on_linux() {
        let ctx = ctx_with(MapEnv::new());
        let x11 = detect_limits(
            &ctx,
            &cli(DisplayKind::X11, None, None, SshFacts::default()),
            &UnlockWindow::Closed,
        );
        assert!(x11.contains(&MachineLimit::X11Session));
        let xw = detect_limits(
            &ctx,
            &cli(DisplayKind::Xwayland, None, Some(true), SshFacts::default()),
            &UnlockWindow::Closed,
        );
        assert!(xw.contains(&MachineLimit::Xwayland {
            terminal_is_x_client: Some(true)
        }));
        let ctx_runtime = ctx_with(MapEnv::new());
        let fallback = detect_limits(
            &ctx_runtime,
            &cli(DisplayKind::None, None, None, SshFacts::default()),
            &UnlockWindow::Closed,
        );
        assert!(fallback.contains(&MachineLimit::RuntimeDirFallback {
            path: "/tmp/s/run".into()
        }));
    }

    /// O1: the SSH key is a row when it has no passphrase or an agent holds it, never when it has
    /// one and no agent runs; an agent that does not answer leaves `in_agent` unknown.
    #[cfg(unix)]
    #[test]
    fn the_ssh_key_is_a_row_when_something_can_use_it_unasked() {
        let dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let plain = dir.path().join("plain");
        std::fs::write(&plain, UNENCRYPTED).unwrap();
        let locked = dir.path().join("locked");
        std::fs::write(&locked, ENCRYPTED).unwrap();
        let ctx = ctx_with(MapEnv::new());
        let row = |key: &Path, agent: Option<PathBuf>| {
            detect_limits(
                &ctx,
                &cli(
                    DisplayKind::None,
                    None,
                    None,
                    SshFacts {
                        key: Some(key.to_path_buf()),
                        agent,
                    },
                ),
                &UnlockWindow::Closed,
            )
            .into_iter()
            .find(|l| matches!(l, MachineLimit::SshKeyUsable { .. }))
        };
        assert!(matches!(
            row(&plain, None),
            Some(MachineLimit::SshKeyUsable {
                unencrypted: true,
                in_agent: Some(false),
                ..
            })
        ));
        assert_eq!(row(&locked, None), None);
        let body = {
            let mut b = vec![12u8, 0, 0, 0, 1];
            b.extend(s(&public()));
            b.extend(s(b"c"));
            b
        };
        let (_a, sock) = agent(Some(body));
        assert!(matches!(
            row(&locked, Some(sock)),
            Some(MachineLimit::SshKeyUsable {
                unencrypted: false,
                in_agent: Some(true),
                ..
            })
        ));
    }

    /// O1 (d4a-f): a program running as you that sets out to get past the lock is a limit of
    /// every computer, for the CLI and the desktop alike — listed always, as secrets in use are.
    #[test]
    fn programs_that_set_out_to_get_past_the_lock_are_always_a_limit() {
        let ctx = ctx_with(MapEnv::new());
        let cli = LimitClient::Cli {
            terminal: None,
            ssh: SshFacts::default(),
        };
        let desk = LimitClient::Desktop {
            session_events: SessionEvents {
                lock: true,
                sleep: true,
            },
            display: DisplayKind::None,
            ssh: SshFacts::default(),
        };
        for (client, window) in [(cli, UnlockWindow::Closed), (desk, UnlockWindow::App)] {
            assert!(detect_limits(&ctx, &client, &window).contains(&MachineLimit::SameUserPrograms));
        }
    }
}
