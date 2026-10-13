// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Starting a terminal's holder (D.4a T47, T48, T54, T59): this very binary as `apprafter
//! __holder`, the key written to its inherited init pipe and nowhere else; the CLI waits at most
//! [`ACK_TIMEOUT`] for its answer and never fails a command on it.

use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use apprafter_core::vault::DataKey;

use super::protocol::{read_ack, Ack, FrameError, HolderUnavailable, InitHeader};
use super::HOLDER_ARG;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// How long a CLI waits for a new holder's ack (T59).
pub const ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderStarted {
    Started { pid: u32 },
    AlreadyRunning { pid: u32 },
}

/// Start this terminal's holder: this binary, as `apprafter __holder` (HG2: the one place both
/// are named). On Windows it must not run beside another spawn of this process: the holder's two
/// pipe ends are inheritable for the `CreateProcessW` call, and a `std::process::Command` started
/// by another thread in that instant would inherit them too (D.4a Deviations 30).
pub fn start_holder(init: &InitHeader, key: &DataKey) -> Result<HolderStarted, HolderUnavailable> {
    let me = std::env::current_exe().map_err(|_| HolderUnavailable::Timeout)?;
    start_with(&me, init, key)
}

/// [`start_holder`] with another binary: tests only (a test binary is no `apprafter`).
#[doc(hidden)]
pub fn start_with(
    program: &Path,
    init: &InitHeader,
    key: &DataKey,
) -> Result<HolderStarted, HolderUnavailable> {
    #[cfg(unix)]
    return unix::start(program, HOLDER_ARG, init, key);
    #[cfg(windows)]
    return windows::start(program, HOLDER_ARG, init, key);
}

/// The ack, or `None` when none came within `timeout`.
fn read_ack_within<R: Read + Send + 'static>(
    mut r: R,
    timeout: Duration,
) -> Option<Result<Ack, FrameError>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_ack(&mut r));
    });
    rx.recv_timeout(timeout).ok()
}
