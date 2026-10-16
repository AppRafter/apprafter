// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The account password checked by PAM, loaded at run time (D.4b B10, B11; ADR 0069 §3;
//! GOTCHA-326, GOTCHA-327).
//!
//! - **The library.** Linux: the build-time path `APPRAFTER_LIBPAM` when the build had one (the
//!   nix dev shell sets it: NixOS has no global library directory), then `libpam.so.0`; macOS:
//!   `/usr/lib/libpam.2.dylib` (in the dyld shared cache). `dlopen`, never a link: a CLI that
//!   could not start where libpam is absent would trade the whole binary for one slot, so a
//!   missing libpam is `Unavailable::NoLibpam`. Loaded once per backend, never closed.
//! - **The service.** Linux: the first of [`LINUX_SERVICES`] that is a file in one of
//!   [`LINUX_SERVICE_DIRS`] (D.2's rule; NixOS has `login` only). macOS: `chkpasswd`, whose
//!   stack is `pam_opendirectory` alone and asks one hidden prompt (d4b-a's probe: `checkpw`
//!   asks nothing, `passwd` is `pam_permit`, the others carry Kerberos, NTLM or a `sudo_local`
//!   include); without it, `Unavailable::NoCheckRoute`.
//! - **One call.** `pam_start` for the account `getpwuid_r(getuid())` names → `pam_authenticate`
//!   with `PAM_DISALLOW_NULL_AUTHTOK`, which overrides the stack's `nullok` → `pam_end`. No
//!   `pam_acct_mgmt`, `pam_setcred` or `pam_open_session`: nobody is logged in.
//! - **"Verified" means the typed text was checked** (B11): `PAM_SUCCESS`, the conversation
//!   answered exactly one hidden prompt with it, and `PAM_USER` unchanged. The conversation
//!   refuses any other prompt — a second hidden one, one that echoes, Linux-PAM's radio and
//!   binary kinds — and keeps info and error messages. [`decide`] reads what was asked before
//!   PAM's code: a refused prompt is `NotChecked{OtherPrompt}` and no answered prompt
//!   `NotChecked{NoPasswordAsked}`, whatever PAM answered; after our one prompt was answered,
//!   `PAM_AUTH_ERR`, `PAM_PERM_DENIED` (Debian's `pam_faillock` `[default=die]`, a disabled
//!   macOS account) and `PAM_MAXTRIES` are a wrong password. Stated limit (the guide's): a stack
//!   whose password module is `optional` before a `sufficient` one succeeds after a wrong
//!   password.
//! - **Counted** (GOTCHA-326): every `pam_authenticate` that answered anything but
//!   `PAM_SUCCESS` may count toward `pam_faillock`, so it counts toward the back-off, whatever
//!   [`decide`] calls it; a `pam_start` that failed reached no module and does not.
//! - **The two PAMs number their codes differently** ([`LINUX_PAM`], [`OPENPAM`]): Linux-PAM's
//!   `PAM_AUTH_ERR` is 7 and `PAM_CONV_ERR` 19, OpenPAM's (macOS) 9 and 6. The message styles,
//!   `PAM_USER`, `PAM_DISALLOW_NULL_AUTHTOK` and the message limit are the same in both.
//!
//! Memory: the password is the caller's `Zeroizing` string; the conversation copies it once into
//! a `malloc`ed buffer libpam owns and frees from then on (Linux-PAM overwrites it first; OpenPAM:
//! not verified). A refused conversation wipes and frees what it had answered. Nothing here
//! formats the password.

// Windows has no PAM: the conversation's pieces, used by the Unix library alone, are unused there.
#![cfg_attr(not(unix), allow(dead_code))]

use std::ffi::{c_int, CStr, CString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::outcome::{CheckOutcome, NotCheckedWhy, Os, Unavailable};

/// `PAM_PROMPT_ECHO_OFF`, `PAM_ERROR_MSG`, `PAM_TEXT_INFO`.
const PROMPT_ECHO_OFF: c_int = 1;
const ERROR_MSG: c_int = 3;
const TEXT_INFO: c_int = 4;
/// `PAM_USER`.
const PAM_USER: c_int = 2;
/// `PAM_DISALLOW_NULL_AUTHTOK`: overrides the stack's `nullok`.
const DISALLOW_NULL_AUTHTOK: c_int = 1;
/// `PAM_MAX_NUM_MSG`.
const MAX_MESSAGES: c_int = 32;
/// What the conversation keeps of the stack's messages.
const MAX_KEPT_MESSAGES: usize = 16;
const MAX_MESSAGE_CHARS: usize = 512;

/// The PAM services a Linux check may use, in the order tried (D.2's list).
pub(crate) const LINUX_SERVICES: [&str; 3] = ["common-auth", "system-auth", "login"];
/// Where libpam reads a Linux service from, relative to `/`, in its order.
pub(crate) const LINUX_SERVICE_DIRS: [&str; 2] = ["etc/pam.d", "usr/lib/pam.d"];
/// The one macOS service that checks the typed text (GOTCHA-327).
pub(crate) const MACOS_SERVICE: &str = "chkpasswd";

/// One PAM implementation's numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Codes {
    pub(crate) success: c_int,
    pub(crate) perm_denied: c_int,
    pub(crate) auth_err: c_int,
    pub(crate) maxtries: c_int,
    pub(crate) conv_err: c_int,
    pub(crate) buf_err: c_int,
    names: &'static [(c_int, &'static str)],
}

/// Linux-PAM's numbers.
pub(crate) const LINUX_PAM: Codes = Codes {
    success: 0,
    perm_denied: 6,
    auth_err: 7,
    maxtries: 11,
    conv_err: 19,
    buf_err: 5,
    names: &[
        (1, "PAM_OPEN_ERR"),
        (2, "PAM_SYMBOL_ERR"),
        (3, "PAM_SERVICE_ERR"),
        (4, "PAM_SYSTEM_ERR"),
        (5, "PAM_BUF_ERR"),
        (8, "PAM_CRED_INSUFFICIENT"),
        (9, "PAM_AUTHINFO_UNAVAIL"),
        (10, "PAM_USER_UNKNOWN"),
        (19, "PAM_CONV_ERR"),
        (26, "PAM_ABORT"),
    ],
};

/// OpenPAM's (macOS's) numbers.
pub(crate) const OPENPAM: Codes = Codes {
    success: 0,
    perm_denied: 7,
    auth_err: 9,
    maxtries: 8,
    conv_err: 6,
    buf_err: 5,
    names: &[
        (1, "PAM_OPEN_ERR"),
        (2, "PAM_SYMBOL_ERR"),
        (3, "PAM_SERVICE_ERR"),
        (4, "PAM_SYSTEM_ERR"),
        (5, "PAM_BUF_ERR"),
        (6, "PAM_CONV_ERR"),
        (11, "PAM_CRED_INSUFFICIENT"),
        (12, "PAM_AUTHINFO_UNAVAIL"),
        (13, "PAM_USER_UNKNOWN"),
        (26, "PAM_ABORT"),
    ],
};

impl Codes {
    /// The numbers of the PAM `os` ships.
    pub(crate) fn of(os: Os) -> Codes {
        if os == Os::Macos {
            OPENPAM
        } else {
            LINUX_PAM
        }
    }

    fn name(&self, code: c_int) -> &'static str {
        self.names
            .iter()
            .find(|(c, _)| *c == code)
            .map_or("PAM_UNKNOWN", |(_, name)| name)
    }
}

/// One check's side of the conversation: the password, and what the stack asked.
pub(crate) struct Dialogue<'a> {
    password: &'a [u8],
    codes: Codes,
    asked: Mutex<Asked>,
}

/// What a [`Dialogue`] was asked.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Asked {
    /// The first hidden prompt has had the password.
    pub(crate) password_given: bool,
    /// A prompt was refused: a second hidden one, one that echoes, any other kind.
    pub(crate) other_prompt: bool,
    /// The info and error messages, in order.
    pub(crate) messages: Vec<String>,
}

/// What one message gets.
enum Reply {
    Nothing,
    Password,
    Refuse,
}

impl fmt::Debug for Dialogue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dialogue")
            .field("asked", &*self.asked())
            .finish_non_exhaustive()
    }
}

impl<'a> Dialogue<'a> {
    pub(crate) fn new(password: &'a [u8], codes: Codes) -> Dialogue<'a> {
        Dialogue {
            password,
            codes,
            asked: Mutex::default(),
        }
    }

    fn asked(&self) -> MutexGuard<'_, Asked> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The password for the first hidden prompt, nothing for an info or error message (kept,
    /// bounded), a refusal for anything else.
    fn reply(&self, style: c_int, text: Option<&CStr>) -> Reply {
        let mut asked = self.asked();
        match style {
            PROMPT_ECHO_OFF if !asked.password_given => {
                asked.password_given = true;
                Reply::Password
            }
            ERROR_MSG | TEXT_INFO => {
                if asked.messages.len() < MAX_KEPT_MESSAGES {
                    let text = text.map_or_else(String::new, |t| {
                        t.to_string_lossy()
                            .chars()
                            .take(MAX_MESSAGE_CHARS)
                            .collect()
                    });
                    asked.messages.push(text);
                }
                Reply::Nothing
            }
            _ => {
                asked.other_prompt = true;
                Reply::Refuse
            }
        }
    }

    /// A message the conversation could not read, or a panic inside it: refused.
    fn refuse_other(&self) {
        self.asked().other_prompt = true;
    }

    pub(crate) fn into_asked(self) -> Asked {
        self.asked
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// What PAM answered one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    /// `pam_authenticate` ran (`pam_start` succeeded).
    pub(crate) authenticated: bool,
    /// `pam_authenticate`'s code, or `pam_start`'s when it failed.
    pub(crate) code: c_int,
    /// `PAM_USER` once `pam_authenticate` returned.
    pub(crate) user_after: Option<Vec<u8>>,
}

/// The outcome of a check that called libpam, and whether it counts toward the back-off
/// (module docs).
pub(crate) fn decide(
    answer: &Answer,
    user: &[u8],
    asked: &Asked,
    codes: Codes,
) -> (CheckOutcome, bool) {
    if !answer.authenticated {
        let name = codes.name(answer.code);
        return (
            CheckOutcome::SystemError {
                code: answer.code,
                name,
            },
            false,
        );
    }
    let success = answer.code == codes.success;
    let outcome = if asked.other_prompt {
        CheckOutcome::NotChecked(NotCheckedWhy::OtherPrompt)
    } else if !asked.password_given {
        CheckOutcome::NotChecked(NotCheckedWhy::NoPasswordAsked)
    } else if success && answer.user_after.as_deref() == Some(user) {
        CheckOutcome::Verified
    } else if success {
        CheckOutcome::NotChecked(NotCheckedWhy::UserChanged)
    } else if [codes.auth_err, codes.perm_denied, codes.maxtries].contains(&answer.code) {
        CheckOutcome::Wrong
    } else {
        let name = codes.name(answer.code);
        CheckOutcome::SystemError {
            code: answer.code,
            name,
        }
    };
    (outcome, !success)
}

/// libpam as a check calls it: the system's in a real backend, a script in the tests.
pub(crate) trait Library: Send + Sync {
    /// Whether it loads with every function a check calls; asks PAM nothing.
    fn load(&self) -> Result<(), Unavailable>;
    /// `pam_start(service, user)` with `dialogue` as the conversation, then
    /// `pam_authenticate(flags)`, `PAM_USER`, `pam_end`.
    fn authenticate(
        &self,
        service: &CStr,
        user: &CStr,
        flags: c_int,
        dialogue: &Dialogue<'_>,
    ) -> Answer;
}

/// The PAM side of one backend: where its services are, whose numbers it speaks, its library.
pub(crate) struct Pam {
    root: PathBuf,
    os: Os,
    library: Box<dyn Library>,
}

impl Pam {
    /// The system's PAM on `os`: services under `/`, libpam by dlopen.
    pub(crate) fn system(os: Os) -> Pam {
        Pam::with(Path::new("/"), os, system_library(os))
    }

    pub(crate) fn with(root: &Path, os: Os, library: Box<dyn Library>) -> Pam {
        Pam {
            root: root.to_path_buf(),
            os,
            library,
        }
    }

    pub(crate) fn codes(&self) -> Codes {
        Codes::of(self.os)
    }

    /// The library and the service a check would use; asks PAM nothing.
    pub(crate) fn backend(&self) -> Result<&'static str, Unavailable> {
        if self.os == Os::Windows {
            return Err(Unavailable::NotOnThisOs);
        }
        self.library.load()?;
        find_service(&self.root, self.os)
    }

    /// One `pam_authenticate` of `password` for `account` through `service`, and what the
    /// stack asked.
    pub(crate) fn authenticate(
        &self,
        service: &str,
        account: &[u8],
        password: &[u8],
    ) -> (Answer, Asked) {
        let dialogue = Dialogue::new(password, self.codes());
        let answer = match (CString::new(service), CString::new(account)) {
            (Ok(service), Ok(user)) => {
                self.library
                    .authenticate(&service, &user, DISALLOW_NULL_AUTHTOK, &dialogue)
            }
            _ => Answer {
                authenticated: false,
                code: -1,
                user_after: None,
            },
        };
        (answer, dialogue.into_asked())
    }
}

/// The service a check on `os` uses, under `root` (module docs).
pub(crate) fn find_service(root: &Path, os: Os) -> Result<&'static str, Unavailable> {
    match os {
        Os::Linux => LINUX_SERVICES
            .into_iter()
            .find(|service| {
                LINUX_SERVICE_DIRS
                    .iter()
                    .any(|dir| root.join(dir).join(service).is_file())
            })
            .ok_or(Unavailable::NoService),
        Os::Macos => root
            .join("etc/pam.d")
            .join(MACOS_SERVICE)
            .is_file()
            .then_some(MACOS_SERVICE)
            .ok_or(Unavailable::NoCheckRoute),
        Os::Windows => Err(Unavailable::NotOnThisOs),
    }
}

/// Where libpam is looked for on `os`, in order (module docs).
pub(crate) fn library_paths(os: Os) -> Vec<&'static str> {
    match os {
        Os::Linux => option_env!("APPRAFTER_LIBPAM")
            .into_iter()
            .filter(|path| !path.is_empty())
            .chain(["libpam.so.0"])
            .collect(),
        Os::Macos => vec!["/usr/lib/libpam.2.dylib"],
        Os::Windows => Vec::new(),
    }
}

/// The system's libpam on `os`.
#[cfg(unix)]
fn system_library(os: Os) -> Box<dyn Library> {
    Box::new(native::SystemLibrary::new(library_paths(os)))
}

/// No PAM on this OS.
#[cfg(not(unix))]
fn system_library(_os: Os) -> Box<dyn Library> {
    Box::new(Absent)
}

/// No PAM (Windows).
#[cfg(not(unix))]
struct Absent;

#[cfg(not(unix))]
impl Library for Absent {
    fn load(&self) -> Result<(), Unavailable> {
        Err(Unavailable::NotOnThisOs)
    }

    fn authenticate(&self, _: &CStr, _: &CStr, _: c_int, _: &Dialogue<'_>) -> Answer {
        Answer {
            authenticated: false,
            code: -1,
            user_after: None,
        }
    }
}

/// The system's libpam by dlopen, and the conversation it calls (Unix).
#[cfg(unix)]
mod native {
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::panic::AssertUnwindSafe;
    use std::sync::OnceLock;

    use super::{Answer, Codes, Dialogue, Library, Reply, MAX_MESSAGES, PAM_USER};
    use crate::outcome::{Os, Unavailable};

    /// `struct pam_message`: the same layout in Linux-PAM and OpenPAM. Built by libpam.
    #[allow(dead_code)]
    #[repr(C)]
    pub(super) struct PamMessage {
        pub(super) msg_style: c_int,
        pub(super) msg: *const c_char,
    }

    /// `struct pam_response`. Allocated here with `calloc`; libpam reads and frees it.
    #[allow(dead_code)]
    #[repr(C)]
    pub(super) struct PamResponse {
        pub(super) resp: *mut c_char,
        pub(super) resp_retcode: c_int,
    }

    /// `struct pam_conv`.
    #[repr(C)]
    struct PamConv {
        conv: Option<ConvFn>,
        appdata_ptr: *mut c_void,
    }

    type ConvFn = unsafe extern "C" fn(
        c_int,
        *mut *const PamMessage,
        *mut *mut PamResponse,
        *mut c_void,
    ) -> c_int;
    type StartFn = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        *const PamConv,
        *mut *mut c_void,
    ) -> c_int;
    type AuthenticateFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
    type GetItemFn = unsafe extern "C" fn(*const c_void, c_int, *mut *const c_void) -> c_int;
    type EndFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

    /// The four functions a check calls.
    #[derive(Clone, Copy)]
    struct Symbols {
        start: StartFn,
        authenticate: AuthenticateFn,
        get_item: GetItemFn,
        end: EndFn,
    }

    /// The system's libpam: the first of `paths` that loads with all four functions, loaded on
    /// first use and never closed.
    pub(super) struct SystemLibrary {
        paths: Vec<&'static str>,
        loaded: OnceLock<Result<Symbols, Vec<String>>>,
    }

    impl SystemLibrary {
        pub(super) fn new(paths: Vec<&'static str>) -> SystemLibrary {
            SystemLibrary {
                paths,
                loaded: OnceLock::new(),
            }
        }

        fn symbols(&self) -> Result<Symbols, Unavailable> {
            self.loaded
                .get_or_init(|| open(&self.paths))
                .clone()
                .map_err(|tried| Unavailable::NoLibpam { tried })
        }
    }

    impl Library for SystemLibrary {
        fn load(&self) -> Result<(), Unavailable> {
            self.symbols().map(drop)
        }

        fn authenticate(
            &self,
            service: &CStr,
            user: &CStr,
            flags: c_int,
            dialogue: &Dialogue<'_>,
        ) -> Answer {
            let failed = |code| Answer {
                authenticated: false,
                code,
                user_after: None,
            };
            let Ok(pam) = self.symbols() else {
                return failed(-1);
            };
            let codes = dialogue.codes;
            let conv = PamConv {
                conv: Some(conversation),
                appdata_ptr: std::ptr::from_ref(dialogue).cast_mut().cast(),
            };
            let mut handle: *mut c_void = std::ptr::null_mut();
            // SAFETY: NUL-terminated service and user; `conv` and `dialogue` outlive pam_end
            // below, after which libpam calls nothing.
            let started =
                unsafe { (pam.start)(service.as_ptr(), user.as_ptr(), &conv, &mut handle) };
            if started != codes.success || handle.is_null() {
                return failed(started);
            }
            // SAFETY: the handle pam_start returned, used on this thread until pam_end.
            let code = unsafe { (pam.authenticate)(handle, flags) };
            let mut item: *const c_void = std::ptr::null();
            // SAFETY: the same handle; PAM_USER's item is null or a NUL-terminated string
            // libpam owns until pam_end.
            let user_after = unsafe {
                ((pam.get_item)(handle, PAM_USER, &mut item) == codes.success && !item.is_null())
                    .then(|| CStr::from_ptr(item.cast()).to_bytes().to_vec())
            };
            // SAFETY: the same handle, ended once.
            let _ = unsafe { (pam.end)(handle, code) };
            Answer {
                authenticated: true,
                code,
                user_after,
            }
        }
    }

    /// The first of `paths` that loads with all four functions; else every path tried.
    fn open(paths: &[&'static str]) -> Result<Symbols, Vec<String>> {
        for path in paths {
            let Ok(name) = CString::new(*path) else {
                continue;
            };
            // SAFETY: a NUL-terminated path; RTLD_NOW resolves libpam's own symbols at once.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                continue;
            }
            let found = [
                c"pam_start",
                c"pam_authenticate",
                c"pam_get_item",
                c"pam_end",
            ]
            // SAFETY: `handle` is a loaded library, and each name is NUL-terminated.
            .map(|symbol| unsafe { libc::dlsym(handle, symbol.as_ptr()) });
            if found.iter().any(|f| f.is_null()) {
                continue;
            }
            // SAFETY: each pointer is the named libpam function, whose C signature these types
            // spell (pam_start(3), pam_authenticate(3), pam_get_item(3), pam_end(3)).
            return Ok(unsafe {
                Symbols {
                    start: std::mem::transmute::<*mut c_void, StartFn>(found[0]),
                    authenticate: std::mem::transmute::<*mut c_void, AuthenticateFn>(found[1]),
                    get_item: std::mem::transmute::<*mut c_void, GetItemFn>(found[2]),
                    end: std::mem::transmute::<*mut c_void, EndFn>(found[3]),
                }
            });
        }
        Err(paths.iter().map(|path| (*path).to_owned()).collect())
    }

    /// The conversation libpam calls (the module docs give its rules). Never unwinds into C.
    ///
    /// # Safety
    ///
    /// As pam_conv(3) calls it: `messages` holds `num` pointers to messages, `responses` is
    /// writable, `appdata` is the `Dialogue` given to pam_start (or null).
    pub(super) unsafe extern "C" fn conversation(
        num: c_int,
        messages: *mut *const PamMessage,
        responses: *mut *mut PamResponse,
        appdata: *mut c_void,
    ) -> c_int {
        if appdata.is_null() {
            return Codes::of(Os::this()).conv_err;
        }
        // SAFETY: `appdata` is the `Dialogue` `SystemLibrary::authenticate` gave pam_start; it
        // outlives pam_end, after which libpam calls nothing.
        let dialogue = unsafe { &*appdata.cast::<Dialogue<'_>>() };
        let refused = dialogue.codes.conv_err;
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: the caller's contract; `converse` checks both pointers for null.
            unsafe { converse(dialogue, num, messages, responses) }
        }))
        .unwrap_or_else(|_| {
            dialogue.refuse_other();
            refused
        })
    }

    /// # Safety
    ///
    /// `messages` is null or holds `num` pointers to messages libpam owns; `responses` is null
    /// or writable.
    unsafe fn converse(
        dialogue: &Dialogue<'_>,
        num: c_int,
        messages: *mut *const PamMessage,
        responses: *mut *mut PamResponse,
    ) -> c_int {
        let codes = dialogue.codes;
        if messages.is_null() || responses.is_null() || !(1..=MAX_MESSAGES).contains(&num) {
            dialogue.refuse_other();
            return codes.conv_err;
        }
        let count = num as usize;
        // SAFETY: calloc of `count` zeroed answers; libpam frees the array and each `resp`.
        let out = unsafe { libc::calloc(count, std::mem::size_of::<PamResponse>()) }
            .cast::<PamResponse>();
        if out.is_null() {
            return codes.buf_err;
        }
        for i in 0..count {
            // SAFETY: `i < num`, and libpam passes `num` message pointers.
            let message = unsafe { *messages.add(i) };
            let reply = if message.is_null() {
                dialogue.refuse_other();
                Reply::Refuse
            } else {
                // SAFETY: a message libpam owns; `msg` is null or NUL-terminated.
                let (style, text) = unsafe { ((*message).msg_style, (*message).msg) };
                // SAFETY: as above.
                let text = (!text.is_null()).then(|| unsafe { CStr::from_ptr(text) });
                dialogue.reply(style, text)
            };
            match reply {
                Reply::Nothing => {}
                Reply::Password => {
                    let copy = c_copy(dialogue.password);
                    if copy.is_null() {
                        // SAFETY: `out` holds `count` answers this call filled or zeroed.
                        unsafe { discard(out, count) };
                        return codes.buf_err;
                    }
                    // SAFETY: `i < count`: an answer of the array just allocated.
                    unsafe { (*out.add(i)).resp = copy };
                }
                Reply::Refuse => {
                    // SAFETY: as above.
                    unsafe { discard(out, count) };
                    return codes.conv_err;
                }
            }
        }
        // SAFETY: `responses` is the writable place libpam gave.
        unsafe { *responses = out };
        codes.success
    }

    /// A `malloc`ed, NUL-terminated copy of `bytes`, for libpam to own; null when out of memory.
    fn c_copy(bytes: &[u8]) -> *mut c_char {
        // SAFETY: malloc of `len + 1` bytes, exactly that many written.
        unsafe {
            let buffer = libc::malloc(bytes.len() + 1).cast::<u8>();
            if !buffer.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len());
                *buffer.add(bytes.len()) = 0;
            }
            buffer.cast()
        }
    }

    /// Wipes and frees every answer in `out`, then `out`.
    ///
    /// # Safety
    ///
    /// `out` is a `calloc`ed array of `count` answers, each `resp` null or a `malloc`ed string.
    unsafe fn discard(out: *mut PamResponse, count: usize) {
        // SAFETY: the caller's contract.
        unsafe {
            for i in 0..count {
                let resp = (*out.add(i)).resp;
                if !resp.is_null() {
                    for j in 0..libc::strlen(resp) {
                        std::ptr::write_volatile(resp.add(j), 0);
                    }
                    libc::free(resp.cast());
                }
            }
            libc::free(out.cast());
        }
    }
}

/// libpam from a script, for this module's tests and `check`'s: never the system's.
#[cfg(test)]
pub(crate) mod scripted {
    use std::ffi::{c_int, CStr, CString};
    use std::sync::{Arc, Mutex};

    use super::{Answer, Dialogue, Library, Reply, LINUX_PAM, PROMPT_ECHO_OFF, TEXT_INFO};
    use crate::outcome::Unavailable;

    /// What the scripted libpam sends, in order.
    #[derive(Debug, Clone)]
    pub(crate) enum Step {
        /// A hidden prompt.
        Hidden,
        /// A prompt that echoes.
        Echo,
        /// An info message.
        Info(&'static str),
    }

    /// libpam from a script, recording each call: `PAM_SUCCESS` when its first hidden prompt
    /// got `right` and nothing was refused, Linux-PAM's `PAM_AUTH_ERR` otherwise, and `unasked`
    /// when no hidden prompt was answered (a `pam_permit` stack: success).
    pub(crate) struct ScriptedLibrary {
        pub(crate) steps: Vec<Step>,
        pub(crate) right: &'static str,
        pub(crate) unasked: c_int,
        /// `PAM_USER` afterwards, when a module changed it.
        pub(crate) user_after: Option<&'static str>,
        pub(crate) loads: bool,
        calls: Mutex<Vec<(String, String, c_int)>>,
    }

    impl ScriptedLibrary {
        pub(crate) fn new(steps: Vec<Step>, right: &'static str) -> ScriptedLibrary {
            ScriptedLibrary {
                steps,
                right,
                unasked: LINUX_PAM.success,
                user_after: None,
                loads: true,
                calls: Mutex::default(),
            }
        }

        /// Every `authenticate`: (service, account, flags).
        pub(crate) fn calls(&self) -> Vec<(String, String, c_int)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Library for Arc<ScriptedLibrary> {
        fn load(&self) -> Result<(), Unavailable> {
            if self.loads {
                Ok(())
            } else {
                Err(Unavailable::NoLibpam {
                    tried: vec!["scripted".to_owned()],
                })
            }
        }

        fn authenticate(
            &self,
            service: &CStr,
            user: &CStr,
            flags: c_int,
            dialogue: &Dialogue<'_>,
        ) -> Answer {
            self.calls.lock().unwrap().push((
                service.to_string_lossy().into_owned(),
                user.to_string_lossy().into_owned(),
                flags,
            ));
            let mut right = None;
            for step in &self.steps {
                match step {
                    Step::Hidden => {
                        if let Reply::Password = dialogue.reply(PROMPT_ECHO_OFF, None) {
                            right.get_or_insert(dialogue.password == self.right.as_bytes());
                        }
                    }
                    Step::Echo => {
                        let _ = dialogue.reply(2, None);
                    }
                    Step::Info(text) => {
                        let text = CString::new(*text).unwrap();
                        let _ = dialogue.reply(TEXT_INFO, Some(&text));
                    }
                }
            }
            let refused = dialogue.asked().other_prompt;
            let code = match right {
                _ if refused => LINUX_PAM.auth_err,
                Some(true) => LINUX_PAM.success,
                Some(false) => LINUX_PAM.auth_err,
                None => self.unasked,
            };
            let user_after = self
                .user_after
                .map_or_else(|| user.to_bytes().to_vec(), |u| u.as_bytes().to_vec());
            Answer {
                authenticated: true,
                code,
                user_after: Some(user_after),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::scripted::{ScriptedLibrary, Step};
    use super::*;

    const RIGHT: &str = "correct horse battery staple";

    fn answer(code: c_int, user: Option<&str>) -> Answer {
        Answer {
            authenticated: true,
            code,
            user_after: user.map(|u| u.as_bytes().to_vec()),
        }
    }

    fn asked(password_given: bool, other_prompt: bool) -> Asked {
        Asked {
            password_given,
            other_prompt,
            messages: Vec::new(),
        }
    }

    /// A root holding these files (paths relative to it).
    fn root_with(files: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for file in files {
            let path = root.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "auth required pam_unix.so\n").unwrap();
        }
        root
    }

    #[test]
    fn verified_needs_success_our_one_prompt_and_the_same_user() {
        for codes in [LINUX_PAM, OPENPAM] {
            let outcome = decide(
                &answer(0, Some("walker")),
                b"walker",
                &asked(true, false),
                codes,
            );
            assert_eq!(outcome, (CheckOutcome::Verified, false));
        }
    }

    #[test]
    fn success_without_our_prompt_is_not_checked() {
        let outcome = decide(
            &answer(0, Some("walker")),
            b"walker",
            &asked(false, false),
            LINUX_PAM,
        );
        let unasked = CheckOutcome::NotChecked(NotCheckedWhy::NoPasswordAsked);
        assert_eq!(outcome, (unasked, false));
    }

    #[test]
    fn success_for_another_user_is_not_verified() {
        for user in [Some("root"), None] {
            let outcome = decide(&answer(0, user), b"walker", &asked(true, false), LINUX_PAM);
            let changed = CheckOutcome::NotChecked(NotCheckedWhy::UserChanged);
            assert_eq!(outcome, (changed, false), "{user:?}");
        }
    }

    #[test]
    fn wrong_is_auth_err_perm_denied_or_maxtries_on_both_pams() {
        for (codes, wrong) in [(LINUX_PAM, [7, 6, 11]), (OPENPAM, [9, 7, 8])] {
            for code in wrong {
                let outcome = decide(
                    &answer(code, Some("walker")),
                    b"walker",
                    &asked(true, false),
                    codes,
                );
                assert_eq!(outcome, (CheckOutcome::Wrong, true), "{code}");
            }
        }
    }

    #[test]
    fn any_other_prompt_is_not_checked_whatever_pam_answered() {
        let other = CheckOutcome::NotChecked(NotCheckedWhy::OtherPrompt);
        let failed = decide(
            &answer(7, Some("walker")),
            b"walker",
            &asked(true, true),
            LINUX_PAM,
        );
        assert_eq!(failed, (other.clone(), true));
        let passed = decide(
            &answer(0, Some("walker")),
            b"walker",
            &asked(true, true),
            LINUX_PAM,
        );
        assert_eq!(passed, (other, false));
    }

    /// A `requisite` module that refused before the password module asked (a faillock-locked
    /// account under some stacks): not checked, and counted, as PAM failed.
    #[test]
    fn a_failure_without_our_prompt_is_not_checked_and_counted() {
        let outcome = decide(
            &answer(6, Some("walker")),
            b"walker",
            &asked(false, false),
            LINUX_PAM,
        );
        let unasked = CheckOutcome::NotChecked(NotCheckedWhy::NoPasswordAsked);
        assert_eq!(outcome, (unasked, true));
    }

    #[test]
    fn other_codes_are_named_counted_system_errors() {
        let error = |code, name| (CheckOutcome::SystemError { code, name }, true);
        let given = asked(true, false);
        assert_eq!(
            decide(&answer(9, None), b"walker", &given, LINUX_PAM),
            error(9, "PAM_AUTHINFO_UNAVAIL")
        );
        assert_eq!(
            decide(&answer(12, None), b"walker", &given, OPENPAM),
            error(12, "PAM_AUTHINFO_UNAVAIL")
        );
        assert_eq!(
            decide(&answer(99, None), b"walker", &given, LINUX_PAM),
            error(99, "PAM_UNKNOWN")
        );
    }

    #[test]
    fn a_pam_start_that_failed_is_a_system_error_not_counted() {
        let start = Answer {
            authenticated: false,
            code: 3,
            user_after: None,
        };
        let error = CheckOutcome::SystemError {
            code: 3,
            name: "PAM_SERVICE_ERR",
        };
        assert_eq!(
            decide(&start, b"walker", &Asked::default(), LINUX_PAM),
            (error, false)
        );
    }

    #[test]
    fn messages_are_kept_bounded_and_lossy() {
        let dialogue = Dialogue::new(RIGHT.as_bytes(), LINUX_PAM);
        let long = CString::new("x".repeat(600)).unwrap();
        let lossy = CString::new(vec![b'a', 0xff, b'b']).unwrap();
        for i in 0..20 {
            let text = if i == 0 { &long } else { &lossy };
            assert!(matches!(
                dialogue.reply(TEXT_INFO, Some(text)),
                Reply::Nothing
            ));
        }
        let kept = dialogue.into_asked();
        assert_eq!(kept.messages.len(), 16);
        assert_eq!(kept.messages[0].chars().count(), 512);
        assert_eq!(kept.messages[1], "a\u{fffd}b");
    }

    #[test]
    fn no_service_file_is_no_service() {
        assert_eq!(
            find_service(root_with(&[]).path(), Os::Linux),
            Err(Unavailable::NoService)
        );
        let others = root_with(&["etc/pam.d/other", "etc/pam.d/sudo", "usr/lib/pam.d/su"]);
        assert_eq!(
            find_service(others.path(), Os::Linux),
            Err(Unavailable::NoService)
        );
        assert_eq!(
            find_service(others.path(), Os::Windows),
            Err(Unavailable::NotOnThisOs)
        );
    }

    #[test]
    fn the_earlier_service_wins_whichever_directory_holds_it() {
        for (files, service) in [
            (
                ["etc/pam.d/login", "usr/lib/pam.d/common-auth"],
                "common-auth",
            ),
            (
                ["usr/lib/pam.d/login", "etc/pam.d/system-auth"],
                "system-auth",
            ),
            (
                ["etc/pam.d/common-auth", "etc/pam.d/system-auth"],
                "common-auth",
            ),
        ] {
            let root = root_with(&files);
            assert_eq!(
                find_service(root.path(), Os::Linux),
                Ok(service),
                "{files:?}"
            );
        }
        let nixos = root_with(&["etc/pam.d/login"]);
        assert_eq!(find_service(nixos.path(), Os::Linux), Ok("login"));
    }

    #[test]
    fn the_mac_route_is_chkpasswd_and_nothing_else() {
        let others = root_with(&[
            "etc/pam.d/checkpw",
            "etc/pam.d/passwd",
            "etc/pam.d/login",
            "etc/pam.d/authorization",
        ]);
        assert_eq!(
            find_service(others.path(), Os::Macos),
            Err(Unavailable::NoCheckRoute)
        );
        let mac = root_with(&["etc/pam.d/chkpasswd"]);
        assert_eq!(find_service(mac.path(), Os::Macos), Ok("chkpasswd"));
        assert_eq!(Codes::of(Os::Macos), OPENPAM);
        assert_eq!(Codes::of(Os::Linux), LINUX_PAM);
        assert_eq!(library_paths(Os::Macos), ["/usr/lib/libpam.2.dylib"]);
    }

    #[test]
    fn the_build_time_libpam_is_tried_first_when_the_build_had_one() {
        match option_env!("APPRAFTER_LIBPAM").filter(|path| !path.is_empty()) {
            Some(path) => {
                assert_eq!(library_paths(Os::Linux), [path, "libpam.so.0"]);
                assert!(
                    Path::new(path).is_file(),
                    "the dev shell's libpam is there: {path}"
                );
            }
            None => assert_eq!(library_paths(Os::Linux), ["libpam.so.0"]),
        }
        assert!(library_paths(Os::Windows).is_empty());
    }

    #[test]
    fn authenticate_passes_disallow_null_authtok_the_service_and_the_account() {
        let library = Arc::new(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        let root = root_with(&["etc/pam.d/common-auth"]);
        let pam = Pam::with(root.path(), Os::Linux, Box::new(Arc::clone(&library)));
        assert_eq!(pam.backend(), Ok("common-auth"));
        let (answer, asked) = pam.authenticate("common-auth", b"walker", RIGHT.as_bytes());
        assert_eq!(answer.code, 0);
        assert!(asked.password_given && !asked.other_prompt, "{asked:?}");
        let call = ("common-auth".to_owned(), "walker".to_owned(), 1);
        assert_eq!(library.calls(), [call]);
    }

    #[test]
    fn windows_has_no_pam() {
        let library = Arc::new(ScriptedLibrary::new(vec![Step::Hidden], RIGHT));
        let root = root_with(&["etc/pam.d/common-auth"]);
        let pam = Pam::with(root.path(), Os::Windows, Box::new(library));
        assert_eq!(pam.backend(), Err(Unavailable::NotOnThisOs));
    }

    /// The scripted libpam's info and echo steps through `Pam::authenticate`: the message is
    /// kept, the echoing prompt refused, and the stack's failure reads not checked, counted
    /// (critic round: without a test that builds them, `Step::Info` and `Step::Echo` are dead).
    #[test]
    fn the_scripted_library_plays_info_and_echo_steps() {
        let steps = vec![Step::Info("Place your finger"), Step::Echo, Step::Hidden];
        let library = Arc::new(ScriptedLibrary::new(steps, RIGHT));
        let root = root_with(&["etc/pam.d/common-auth"]);
        let pam = Pam::with(root.path(), Os::Linux, Box::new(Arc::clone(&library)));
        let (answer, asked) = pam.authenticate("common-auth", b"walker", RIGHT.as_bytes());
        assert_eq!(answer.code, LINUX_PAM.auth_err);
        assert!(asked.other_prompt && asked.password_given, "{asked:?}");
        assert_eq!(asked.messages, ["Place your finger"]);
        let other = CheckOutcome::NotChecked(NotCheckedWhy::OtherPrompt);
        assert_eq!(decide(&answer, b"walker", &asked, LINUX_PAM), (other, true));
    }

    /// The system's library type over paths that do not exist: it names each and loads none.
    #[cfg(unix)]
    #[test]
    fn a_library_that_does_not_load_names_every_path_tried() {
        let paths = vec!["/nonexistent/libpam.so.0", "libapprafter-no-such-pam.so.0"];
        let library = native::SystemLibrary::new(paths.clone());
        let tried = paths.iter().map(|p| (*p).to_owned()).collect();
        assert_eq!(library.load(), Err(Unavailable::NoLibpam { tried }));
    }

    /// The conversation called as libpam calls it, with `malloc`ed answers freed here.
    #[cfg(unix)]
    mod conversation {
        use std::ffi::{c_int, c_void, CStr, CString};

        use super::super::native::{conversation, PamMessage, PamResponse};
        use super::super::{Asked, Dialogue, ERROR_MSG, LINUX_PAM, PROMPT_ECHO_OFF, TEXT_INFO};

        const RIGHT: &str = "correct horse battery staple";
        const PROMPT_ECHO_ON: c_int = 2;

        /// `styles`' messages through the conversation: its code, each answer (`None` for
        /// none), and what it recorded.
        fn converse_with(styles: &[(c_int, &str)]) -> (c_int, Vec<Option<String>>, Asked) {
            let texts: Vec<CString> = styles
                .iter()
                .map(|(_, t)| CString::new(*t).unwrap())
                .collect();
            let messages: Vec<PamMessage> = styles
                .iter()
                .zip(&texts)
                .map(|((style, _), text)| PamMessage {
                    msg_style: *style,
                    msg: text.as_ptr(),
                })
                .collect();
            let mut pointers: Vec<*const PamMessage> =
                messages.iter().map(std::ptr::from_ref).collect();
            let dialogue = Dialogue::new(RIGHT.as_bytes(), LINUX_PAM);
            let appdata = std::ptr::from_ref(&dialogue).cast_mut().cast::<c_void>();
            let mut out: *mut PamResponse = std::ptr::null_mut();
            let num = c_int::try_from(pointers.len()).unwrap();
            // SAFETY: as libpam calls it — `num` valid messages, a place for the answers, the
            // dialogue as appdata, all alive across the call.
            let code = unsafe { conversation(num, pointers.as_mut_ptr(), &mut out, appdata) };
            let mut answers = Vec::new();
            if !out.is_null() {
                // SAFETY: the conversation's `calloc`ed array of `styles.len()` answers, each
                // `resp` null or `malloc`ed, freed once here.
                unsafe {
                    for i in 0..styles.len() {
                        let resp = (*out.add(i)).resp;
                        let text = (!resp.is_null())
                            .then(|| CStr::from_ptr(resp).to_string_lossy().into_owned());
                        answers.push(text);
                        libc::free(resp.cast());
                    }
                    libc::free(out.cast());
                }
            }
            (code, answers, dialogue.into_asked())
        }

        #[test]
        fn one_hidden_prompt_gets_the_password_and_messages_are_kept() {
            let (code, answers, asked) = converse_with(&[
                (TEXT_INFO, "Place your finger"),
                (PROMPT_ECHO_OFF, "Password: "),
                (ERROR_MSG, "Last failure: never"),
            ]);
            assert_eq!(code, 0);
            assert_eq!(answers, [None, Some(RIGHT.to_owned()), None]);
            let messages = vec![
                "Place your finger".to_owned(),
                "Last failure: never".to_owned(),
            ];
            assert_eq!(
                asked,
                Asked {
                    password_given: true,
                    other_prompt: false,
                    messages
                }
            );
        }

        #[test]
        fn a_second_hidden_prompt_is_refused_and_nothing_is_answered() {
            let (code, answers, asked) = converse_with(&[
                (PROMPT_ECHO_OFF, "Password: "),
                (PROMPT_ECHO_OFF, "Second password: "),
            ]);
            assert_eq!(code, LINUX_PAM.conv_err);
            assert!(
                answers.is_empty(),
                "wiped and freed, none handed over: {answers:?}"
            );
            assert!(asked.password_given && asked.other_prompt, "{asked:?}");
        }

        #[test]
        fn an_echoing_prompt_never_gets_the_password() {
            let (code, answers, asked) = converse_with(&[(PROMPT_ECHO_ON, "One-time code: ")]);
            assert_eq!(code, LINUX_PAM.conv_err);
            assert!(answers.is_empty(), "{answers:?}");
            assert!(!asked.password_given && asked.other_prompt, "{asked:?}");
        }

        #[test]
        fn radio_binary_and_unknown_styles_are_refused() {
            for style in [5, 7, 42] {
                let (code, _, asked) = converse_with(&[(style, "?")]);
                assert_eq!(code, LINUX_PAM.conv_err, "{style}");
                assert!(asked.other_prompt, "{style}");
            }
        }

        #[test]
        fn a_message_count_out_of_bounds_is_refused() {
            let many = [(TEXT_INFO, "info"); 33];
            assert_eq!(converse_with(&many).0, LINUX_PAM.conv_err);
            assert_eq!(converse_with(&[]).0, LINUX_PAM.conv_err);
        }

        #[test]
        fn a_conversation_without_its_dialogue_is_refused() {
            let mut out: *mut PamResponse = std::ptr::null_mut();
            let mut none: Vec<*const PamMessage> = Vec::new();
            // SAFETY: a null appdata is refused before any message is read.
            let code =
                unsafe { conversation(1, none.as_mut_ptr(), &mut out, std::ptr::null_mut()) };
            assert_ne!(code, 0);
            assert!(out.is_null());
        }
    }
}
