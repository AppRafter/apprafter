// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Unix start (holder §1.2): `env_clear`, cwd `/`, stdin the init pipe, stdout the ack pipe,
//! stderr `/dev/null`. No `pre_exec` and no `process_group(0)`: the holder records the session it
//! was born in, then leaves it itself (a group leader's `setsid` would fail).

use std::path::Path;
use std::process::{Command, Stdio};

use apprafter_core::vault::DataKey;

use super::{read_ack_within, HolderStarted, ACK_TIMEOUT};
use crate::holder::protocol::{write_init, Ack, HolderUnavailable, InitHeader};

pub(super) fn start(
    program: &Path,
    arg: &str,
    init: &InitHeader,
    key: &DataKey,
) -> Result<HolderStarted, HolderUnavailable> {
    let mut child = Command::new(program)
        .arg(arg)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| HolderUnavailable::Timeout)?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(HolderUnavailable::Timeout);
    };
    // The key's one trip (T48): from its buffer into the pipe, which is unbuffered.
    let wrote = write_init(&mut stdin, init, key);
    drop(stdin);
    if wrote.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(HolderUnavailable::Timeout);
    }
    match read_ack_within(stdout, ACK_TIMEOUT) {
        // The holder lives on; it is reaped by whoever inherits it once this CLI exits.
        Some(Ok(Ack::Ready { pid, .. })) => Ok(HolderStarted::Started { pid }),
        Some(Ok(Ack::AlreadyRunning { pid })) => {
            let _ = child.wait();
            Ok(HolderStarted::AlreadyRunning { pid })
        }
        Some(Ok(Ack::Unavailable(u))) => {
            let _ = child.wait();
            Err(u)
        }
        Some(Err(_)) | None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(HolderUnavailable::Timeout)
        }
    }
}
