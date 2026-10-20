// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! [`ScriptedAccountPassword`]: the stand-in for the account-password backend and this
//! computer's facts (D.4b B15; feature `test-support`, turned on by dev-dependency edges only —
//! G-OS2). It reads no real account, asks no PAM and touches no back-off record: every answer
//! comes from a [`Script`], each call is recorded, an entry is refused as the real backend
//! refuses it (empty, or holding a NUL) without using an answer, and it plays one OS. A check
//! past the script's last answer panics, naming [`STAND_IN_MARK`].
//!
//! `platform-cli`'s `test-os-slots` feature (d4b-e1a) builds it from the file
//! `APPRAFTER_TEST_OS_SLOTS` names ([`ScriptedAccountPassword::from_file`]); each call is then
//! also appended to `<file>.calls` (the password's length, never its text). The script, one line
//! each, `#` comments:
//!
//! ```text
//! os linux|macos|windows
//! account <name>|none
//! machine-id <32 lowercase hex>|missing|empty|malformed   (linux: those characters; macos: 16 bytes)
//! host <name>|none
//! environment plain|container|wsl|container+wsl
//! status usable|locked|empty|unknown [day YYYY-MM-DD|at <unix seconds>]
//! backend ready|no-libpam|no-service|no-check-route|no-account|no-new-privs|not-on-this-os
//! backoff clear|no-record|wait <seconds> recent-failure|two-in-an-hour|another-check-running
//! check verified-if <password>|wrong|not-checked no-password-asked|other-prompt|user-changed|system-error <code>
//! ```
//!
//! Each `check` line answers one check, in order (`verified-if`: `Verified` for exactly that
//! text, else `Wrong`; the text is the rest of the line, trimmed). Every other keyword sets a
//! standing answer, the last line of it winning. Defaults: `os linux`, `account walker`,
//! [`SCRIPTED_MACHINE_ID`], `host scripted-host`, `environment plain`, `status usable`,
//! `backend ready`, `backoff clear`. `os windows` and `account none` answer as the real backend
//! does — `NotOnThisOs`, `NoAccount`, an unknown status — whatever the `backend`, `backoff` and
//! `status` lines say, and an `another-check-running` wait has no end time, as there (critic
//! round).

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use zeroize::Zeroizing;

use crate::account::parse_day;
use crate::{
    AccountPassword, BackoffState, BackoffWait, CheckOutcome, Environment, HostFacts, InputRefused,
    MachineId, NoMachineId, NotCheckedWhy, Os, PasswordChanged, PasswordCheck, PasswordState,
    PasswordStatus, Unavailable, WaitWhy,
};

/// A text only the stand-in carries: d4b-e1a's G-OS3 looks for it in a release binary.
pub const STAND_IN_MARK: &str = "apprafter-os-slots scripted stand-in";

/// The machine id a script has unless it names one.
pub const SCRIPTED_MACHINE_ID: &str = "5c1c18715c1c18715c1c18715c1c1871";

/// One `check` line's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptedCheck {
    /// `Verified` for exactly this text, `Wrong` for any other.
    VerifiedIf(String),
    Answer(CheckOutcome),
}

/// The `backoff` line's answer; a wait is relative to the `now` each call is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptedBackoff {
    Clear,
    Wait { seconds: u64, why: WaitWhy },
}

/// Every answer a [`ScriptedAccountPassword`] gives (module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub os: Os,
    pub account: Option<String>,
    /// As the script wrote it (32 lowercase hexadecimal characters), or why there is none.
    pub machine_id: Result<String, NoMachineId>,
    pub host: Option<String>,
    pub environment: Environment,
    pub status: PasswordStatus,
    pub backend: Result<(), Unavailable>,
    pub backoff: Result<ScriptedBackoff, Unavailable>,
    pub checks: Vec<ScriptedCheck>,
}

impl Default for Script {
    fn default() -> Script {
        Script {
            os: Os::Linux,
            account: Some("walker".to_owned()),
            machine_id: Ok(SCRIPTED_MACHINE_ID.to_owned()),
            host: Some("scripted-host".to_owned()),
            environment: Environment::default(),
            status: PasswordStatus {
                state: PasswordState::Usable,
                changed: PasswordChanged::Unknown,
            },
            backend: Ok(()),
            backoff: Ok(ScriptedBackoff::Clear),
            checks: Vec::new(),
        }
    }
}

/// A script line that does not read (`line` 0: the file could not be read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    pub line: usize,
    pub text: String,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{STAND_IN_MARK}: line {} of the script: {:?}",
            self.line, self.text
        )
    }
}

impl std::error::Error for ScriptError {}

impl Script {
    /// Reads a script (module docs).
    pub fn parse(text: &str) -> Result<Script, ScriptError> {
        let mut script = Script::default();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = || ScriptError {
                line: index + 1,
                text: raw.to_owned(),
            };
            let (keyword, rest) = line.split_once(' ').unwrap_or((line, ""));
            let words: Vec<&str> = rest.split_whitespace().collect();
            match (keyword, words.as_slice()) {
                ("os", ["linux"]) => script.os = Os::Linux,
                ("os", ["macos"]) => script.os = Os::Macos,
                ("os", ["windows"]) => script.os = Os::Windows,
                ("account", ["none"]) => script.account = None,
                ("account", [name]) => script.account = Some((*name).to_owned()),
                ("machine-id", ["missing"]) => script.machine_id = Err(NoMachineId::Missing),
                ("machine-id", ["empty"]) => script.machine_id = Err(NoMachineId::Empty),
                ("machine-id", ["malformed"]) => script.machine_id = Err(NoMachineId::Malformed),
                ("machine-id", [hex]) if is_machine_hex(hex) => {
                    script.machine_id = Ok((*hex).to_owned())
                }
                ("host", ["none"]) => script.host = None,
                ("host", [name]) => script.host = Some((*name).to_owned()),
                ("environment", [kind]) => {
                    script.environment = environment_of(kind).ok_or_else(bad)?
                }
                ("status", [state, when @ ..]) => {
                    script.status = PasswordStatus {
                        state: state_of(state).ok_or_else(bad)?,
                        changed: changed_of(when).ok_or_else(bad)?,
                    }
                }
                ("backend", ["ready"]) => script.backend = Ok(()),
                ("backend", [why]) => script.backend = Err(unavailable_of(why).ok_or_else(bad)?),
                ("backoff", ["clear"]) => script.backoff = Ok(ScriptedBackoff::Clear),
                ("backoff", ["no-record"]) => {
                    script.backoff = Err(Unavailable::NoBackoffRecord {
                        dir: PathBuf::from("/run/user/scripted"),
                    })
                }
                ("backoff", ["wait", seconds, why]) => {
                    script.backoff = Ok(ScriptedBackoff::Wait {
                        seconds: seconds.parse().map_err(|_| bad())?,
                        why: wait_why_of(why).ok_or_else(bad)?,
                    })
                }
                ("check", _) if rest.starts_with("verified-if ") => {
                    let text = rest
                        .strip_prefix("verified-if ")
                        .unwrap_or_default()
                        .to_owned();
                    script.checks.push(ScriptedCheck::VerifiedIf(text));
                }
                ("check", ["wrong"]) => script
                    .checks
                    .push(ScriptedCheck::Answer(CheckOutcome::Wrong)),
                ("check", ["not-checked", why]) => {
                    let why = not_checked_of(why).ok_or_else(bad)?;
                    script
                        .checks
                        .push(ScriptedCheck::Answer(CheckOutcome::NotChecked(why)));
                }
                ("check", ["system-error", code]) => {
                    let code = code.parse().map_err(|_| bad())?;
                    let error = CheckOutcome::SystemError {
                        code,
                        name: "PAM_SCRIPTED",
                    };
                    script.checks.push(ScriptedCheck::Answer(error));
                }
                _ => return Err(bad()),
            }
        }
        Ok(script)
    }
}

fn is_machine_hex(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

fn environment_of(word: &str) -> Option<Environment> {
    let (container, wsl) = match word {
        "plain" => (false, false),
        "container" => (true, false),
        "wsl" => (false, true),
        "container+wsl" => (true, true),
        _ => return None,
    };
    Some(Environment { container, wsl })
}

fn state_of(word: &str) -> Option<PasswordState> {
    match word {
        "usable" => Some(PasswordState::Usable),
        "locked" => Some(PasswordState::Locked),
        "empty" => Some(PasswordState::Empty),
        "unknown" => Some(PasswordState::Unknown),
        _ => None,
    }
}

fn changed_of(words: &[&str]) -> Option<PasswordChanged> {
    match words {
        [] => Some(PasswordChanged::Unknown),
        ["day", date] => {
            parse_day(date).map(|days_since_epoch| PasswordChanged::Day { days_since_epoch })
        }
        ["at", seconds] => seconds
            .parse()
            .ok()
            .map(|unix_seconds| PasswordChanged::At { unix_seconds }),
        _ => None,
    }
}

fn unavailable_of(word: &str) -> Option<Unavailable> {
    match word {
        "no-libpam" => Some(Unavailable::NoLibpam {
            tried: vec!["libpam.so.0".to_owned()],
        }),
        "no-service" => Some(Unavailable::NoService),
        "no-check-route" => Some(Unavailable::NoCheckRoute),
        "no-account" => Some(Unavailable::NoAccount),
        "no-new-privs" => Some(Unavailable::NoNewPrivs),
        "not-on-this-os" => Some(Unavailable::NotOnThisOs),
        _ => None,
    }
}

fn wait_why_of(word: &str) -> Option<WaitWhy> {
    match word {
        "recent-failure" => Some(WaitWhy::RecentFailure),
        "two-in-an-hour" => Some(WaitWhy::TwoInAnHour),
        "another-check-running" => Some(WaitWhy::AnotherCheckRunning),
        _ => None,
    }
}

fn not_checked_of(word: &str) -> Option<NotCheckedWhy> {
    match word {
        "no-password-asked" => Some(NotCheckedWhy::NoPasswordAsked),
        "other-prompt" => Some(NotCheckedWhy::OtherPrompt),
        "user-changed" => Some(NotCheckedWhy::UserChanged),
        _ => None,
    }
}

/// One call to the stand-in, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Backend,
    Backoff,
    Status,
    /// A check, and the length of what it was given — never the text.
    Check {
        chars: usize,
    },
    MachineId,
}

/// The stand-in (module docs).
pub struct ScriptedAccountPassword {
    script: Script,
    next_check: Mutex<usize>,
    calls: Mutex<Vec<Call>>,
    log: Option<PathBuf>,
}

impl ScriptedAccountPassword {
    pub fn new(script: Script) -> ScriptedAccountPassword {
        ScriptedAccountPassword {
            script,
            next_check: Mutex::new(0),
            calls: Mutex::new(Vec::new()),
            log: None,
        }
    }

    /// The script in `path`; each call also appended to `<path>.calls`, which starts with a
    /// line naming [`STAND_IN_MARK`] when it is new.
    pub fn from_file(path: &Path) -> Result<ScriptedAccountPassword, ScriptError> {
        let text = std::fs::read_to_string(path).map_err(|e| ScriptError {
            line: 0,
            text: format!("{}: {e}", path.display()),
        })?;
        let script = Script::parse(&text)?;
        let mut log = path.as_os_str().to_owned();
        log.push(".calls");
        let log = PathBuf::from(log);
        if let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(&log) {
            let _ = writeln!(file, "# {STAND_IN_MARK}");
        }
        Ok(ScriptedAccountPassword {
            log: Some(log),
            ..ScriptedAccountPassword::new(script)
        })
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn record(&self, call: Call) {
        self.append(&format!("{call:?}"));
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(call);
    }

    fn append(&self, line: &str) {
        if let Some(path) = &self.log {
            if let Ok(mut file) = OpenOptions::new().append(true).create(true).open(path) {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    /// Whether the real backend would have no slot here whatever the script says: Windows, or no
    /// account (module docs).
    fn plays_no_slot(&self) -> bool {
        self.script.os == Os::Windows || self.script.account.is_none()
    }

    /// The real backend's answer before PAM: no slot on Windows, no account, then the script's
    /// `backend` line.
    fn backend_answer(&self) -> Result<(), Unavailable> {
        if self.script.os == Os::Windows {
            return Err(Unavailable::NotOnThisOs);
        }
        if self.script.account.is_none() {
            return Err(Unavailable::NoAccount);
        }
        self.script.backend.clone()
    }

    fn backoff_at(&self, now: SystemTime) -> Result<BackoffState, Unavailable> {
        if self.plays_no_slot() {
            self.backend_answer()?;
        }
        match &self.script.backoff {
            Ok(ScriptedBackoff::Clear) => Ok(BackoffState::Clear),
            Ok(ScriptedBackoff::Wait { seconds, why }) => Ok(BackoffState::Wait(BackoffWait {
                // Another process's check has no end time; the real backend says `None` too.
                until: (*why != WaitWhy::AnotherCheckRunning)
                    .then(|| now + Duration::from_secs(*seconds)),
                why: *why,
            })),
            Err(why) => Err(why.clone()),
        }
    }

    fn next_answer(&self, password: &str) -> CheckOutcome {
        let mut next = self
            .next_check
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let index = *next;
        *next += 1;
        match self.script.checks.get(index) {
            Some(ScriptedCheck::VerifiedIf(right)) if right.as_str() == password => {
                CheckOutcome::Verified
            }
            Some(ScriptedCheck::VerifiedIf(_)) => CheckOutcome::Wrong,
            Some(ScriptedCheck::Answer(outcome)) => outcome.clone(),
            None => panic!(
                "{STAND_IN_MARK}: the script has no answer for check #{}",
                index + 1
            ),
        }
    }
}

impl AccountPassword for ScriptedAccountPassword {
    fn os(&self) -> Os {
        self.script.os
    }

    fn account_name(&self) -> Option<String> {
        self.script.account.clone()
    }

    fn backend(&self) -> Result<(), Unavailable> {
        self.record(Call::Backend);
        self.backend_answer()
    }

    fn backoff(&self, now: SystemTime) -> Result<BackoffState, Unavailable> {
        self.record(Call::Backoff);
        self.backoff_at(now)
    }

    fn password_status(&self) -> PasswordStatus {
        self.record(Call::Status);
        if self.plays_no_slot() {
            PasswordStatus::UNKNOWN
        } else {
            self.script.status
        }
    }

    fn check(&self, password: &Zeroizing<String>, now: SystemTime) -> PasswordCheck {
        self.record(Call::Check {
            chars: password.chars().count(),
        });
        let outcome = if password.is_empty() {
            CheckOutcome::Refused(InputRefused::Empty)
        } else if password.contains('\0') {
            CheckOutcome::Refused(InputRefused::ContainsNul)
        } else if let Err(why) = self.backend_answer() {
            CheckOutcome::Unavailable(why)
        } else {
            match self.backoff_at(now) {
                Err(why) => CheckOutcome::Unavailable(why),
                Ok(BackoffState::Wait(wait)) => CheckOutcome::Backoff(wait),
                Ok(BackoffState::Clear) => self.next_answer(password),
            }
        };
        PasswordCheck {
            outcome,
            messages: Vec::new(),
        }
    }
}

impl HostFacts for ScriptedAccountPassword {
    fn machine_id(&self) -> Result<MachineId, NoMachineId> {
        self.record(Call::MachineId);
        let hex = self.script.machine_id.clone()?;
        // A null id is none, as the real readers say (machine.rs, macos.rs): the vault refuses
        // it, so the glue meets `Malformed` before any check, never a vault error after one.
        if self.script.os != Os::Windows && hex.bytes().all(|b| b == b'0') {
            return Err(NoMachineId::Malformed);
        }
        let bytes = match self.script.os {
            Os::Macos => hex_bytes(&hex),
            Os::Linux => Some(hex.into_bytes()),
            Os::Windows => return Err(NoMachineId::NotOnThisOs),
        };
        bytes.map(MachineId::new).ok_or(NoMachineId::Malformed)
    }

    fn environment(&self) -> Environment {
        self.script.environment
    }

    fn host_name(&self) -> Option<String> {
        self.script.host.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn secret(text: &str) -> Zeroizing<String> {
        Zeroizing::new(text.to_owned())
    }

    fn scripted(text: &str) -> ScriptedAccountPassword {
        ScriptedAccountPassword::new(Script::parse(text).unwrap())
    }

    #[test]
    fn verified_if_answers_only_the_exact_text() {
        let s = scripted("check verified-if right horse\ncheck verified-if right horse\ncheck verified-if right horse\n");
        assert_eq!(
            s.check(&secret("right horse"), now()).outcome,
            CheckOutcome::Verified
        );
        assert_eq!(
            s.check(&secret("right horse!"), now()).outcome,
            CheckOutcome::Wrong
        );
        assert_eq!(
            s.check(&secret("right"), now()).outcome,
            CheckOutcome::Wrong
        );
    }

    #[test]
    fn an_empty_entry_is_refused_as_the_real_one_and_uses_no_answer() {
        let s = scripted("check verified-if x\n");
        assert_eq!(
            s.check(&secret(""), now()).outcome,
            CheckOutcome::Refused(InputRefused::Empty)
        );
        let nul = s.check(&secret("a\0b"), now()).outcome;
        assert_eq!(nul, CheckOutcome::Refused(InputRefused::ContainsNul));
        assert_eq!(s.check(&secret("x"), now()).outcome, CheckOutcome::Verified);
        let calls = [
            Call::Check { chars: 0 },
            Call::Check { chars: 3 },
            Call::Check { chars: 1 },
        ];
        assert_eq!(s.calls(), calls);
    }

    #[test]
    #[should_panic(expected = "apprafter-os-slots scripted stand-in")]
    fn a_check_past_the_script_panics_naming_the_stand_in() {
        scripted("").check(&secret("x"), now());
    }

    #[test]
    fn the_machine_id_follows_the_os_it_plays() {
        let hex = "00112233445566778899aabbccddeeff";
        let linux = scripted(&format!("machine-id {hex}\n"));
        assert_eq!(linux.machine_id().unwrap().expose(), hex.as_bytes());
        let mac = scripted(&format!("os macos\nmachine-id {hex}\n"));
        let raw: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        assert_eq!(mac.machine_id().unwrap().expose(), raw);
        assert_eq!(
            scripted("machine-id empty\n").machine_id().err(),
            Some(NoMachineId::Empty)
        );
        let windows = scripted("os windows\n");
        assert_eq!(windows.os(), Os::Windows);
        assert_eq!(windows.machine_id().err(), Some(NoMachineId::NotOnThisOs));
        assert_eq!(
            scripted("").machine_id().unwrap().expose(),
            SCRIPTED_MACHINE_ID.as_bytes()
        );
    }

    /// A null id reads as none on both OSes, as the real readers say it (critic round's M4, for
    /// the stand-in too: a script cannot hand the glue an id the vault refuses).
    #[test]
    fn a_null_machine_id_is_none_as_the_real_readers_say() {
        let null = "00000000000000000000000000000000";
        for os in ["linux", "macos"] {
            let s = scripted(&format!("os {os}\nmachine-id {null}\n"));
            assert_eq!(s.machine_id().err(), Some(NoMachineId::Malformed), "{os}");
        }
        let windows = scripted(&format!("os windows\nmachine-id {null}\n"));
        assert_eq!(windows.machine_id().err(), Some(NoMachineId::NotOnThisOs));
    }

    #[test]
    fn standing_answers_follow_the_script() {
        let s = scripted(
            "host none\nenvironment container+wsl\nstatus locked day 2026-10-15\nbackoff wait 600 recent-failure\n",
        );
        assert_eq!(scripted("account none\n").account_name(), None);
        assert_eq!(s.host_name(), None);
        assert_eq!(
            s.environment(),
            Environment {
                container: true,
                wsl: true
            }
        );
        let status = PasswordStatus {
            state: PasswordState::Locked,
            changed: PasswordChanged::Day {
                days_since_epoch: 20_741,
            },
        };
        assert_eq!(s.password_status(), status);
        let wait = BackoffWait {
            until: Some(now() + Duration::from_secs(600)),
            why: WaitWhy::RecentFailure,
        };
        assert_eq!(s.backoff(now()), Ok(BackoffState::Wait(wait)));
        assert_eq!(
            s.check(&secret("x"), now()).outcome,
            CheckOutcome::Backoff(wait),
            "no answer used"
        );
        let unavailable = scripted("backend no-libpam\n");
        let no_libpam = Unavailable::NoLibpam {
            tried: vec!["libpam.so.0".to_owned()],
        };
        assert_eq!(unavailable.backend(), Err(no_libpam.clone()));
        assert_eq!(
            unavailable.check(&secret("x"), now()).outcome,
            CheckOutcome::Unavailable(no_libpam)
        );
        let defaults = scripted("check not-checked other-prompt\n");
        assert_eq!(defaults.account_name().as_deref(), Some("walker"));
        let other = CheckOutcome::NotChecked(NotCheckedWhy::OtherPrompt);
        assert_eq!(defaults.check(&secret("x"), now()).outcome, other);
    }

    /// WI-545: `backend no-new-privs` answers as the real backend under no_new_privs — before
    /// the back-off, so a check in a back-off still reads no_new_privs and uses no answer.
    #[test]
    fn no_new_privs_answers_before_the_back_off() {
        let s = scripted(
            "backend no-new-privs\nbackoff wait 600 recent-failure\ncheck verified-if x\n",
        );
        assert_eq!(s.backend(), Err(Unavailable::NoNewPrivs));
        assert_eq!(
            s.check(&secret("x"), now()).outcome,
            CheckOutcome::Unavailable(Unavailable::NoNewPrivs)
        );
        assert_eq!(s.calls(), [Call::Backend, Call::Check { chars: 1 }]);
    }

    #[test]
    fn windows_and_no_account_answer_as_the_real_backend() {
        for (text, why) in [
            (
                "os windows\ncheck verified-if x\n",
                Unavailable::NotOnThisOs,
            ),
            (
                "account none\ncheck verified-if x\n",
                Unavailable::NoAccount,
            ),
        ] {
            let s = scripted(text);
            assert_eq!(s.backend(), Err(why.clone()), "{text}");
            assert_eq!(s.backoff(now()), Err(why.clone()), "{text}");
            assert_eq!(s.password_status(), PasswordStatus::UNKNOWN, "{text}");
            let check = s.check(&secret("x"), now()).outcome;
            assert_eq!(check, CheckOutcome::Unavailable(why), "{text}");
        }
        let busy = scripted("backoff wait 10 another-check-running\n");
        let running = BackoffWait {
            until: None,
            why: WaitWhy::AnotherCheckRunning,
        };
        assert_eq!(busy.backoff(now()), Ok(BackoffState::Wait(running)));
    }

    #[test]
    fn an_unknown_line_is_an_error_naming_it() {
        let error = Script::parse("os linux\nfrobnicate now\n").unwrap_err();
        assert_eq!(
            error,
            ScriptError {
                line: 2,
                text: "frobnicate now".to_owned()
            }
        );
        assert!(error.to_string().contains(STAND_IN_MARK));
        for bad in [
            "os beos",
            "machine-id 0123",
            "machine-id 0123456789ABCDEF0123456789abcdef",
            "status maybe",
            "backoff wait soon recent-failure",
            "check not-checked maybe",
            "check system-error x",
        ] {
            assert!(Script::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_call_log_never_holds_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("os-slots.script");
        std::fs::write(&path, "# a comment\ncheck verified-if hunter2-secret\n").unwrap();
        let s = ScriptedAccountPassword::from_file(&path).unwrap();
        s.backend().unwrap();
        assert_eq!(
            s.check(&secret("hunter2-secret"), now()).outcome,
            CheckOutcome::Verified
        );
        let log = std::fs::read_to_string(dir.path().join("os-slots.script.calls")).unwrap();
        assert!(!log.contains("hunter2"), "{log}");
        assert!(log.starts_with(&format!("# {STAND_IN_MARK}\n")), "{log}");
        assert!(log.contains("Backend\nCheck { chars: 14 }\n"), "{log}");
    }
}
