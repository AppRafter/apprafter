// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The CLI's `Reporter`: what the core's operations report, printed exactly as the CLI printed
//! it before the core existed. `Stage` / `Progress` are not shown by the commands that use it.
//!
//! The CLI's one `Reporter` (overview §3.7.3, §5): D.3b's target arms and D.3c's doctor use it,
//! and neither creates its own. `target ip`, its first core-backed command, reports nothing.

use std::io::Write;

use apprafter_core::{Event, Reporter, Stream};

/// Prints `Output` bytes to their stream as they arrive, a `Notice` verbatim and a `Warning` as
/// `warning: <message>` on stderr; ignores `Stage` and `Progress`.
// `allow`, not `expect`: whether a never-constructed struct with a trait impl (and the inherent
// fn only that impl calls) is dead differs between toolchains — rustc 1.98 says yes, the dev
// shell's 1.95 says no — so an expectation is unfulfilled on one of them. Remove both `allow`s
// with the first caller.
#[allow(
    dead_code,
    reason = "first caller: D.3b's target arms and D.3c's doctor"
)]
pub(crate) struct CliReporter;

impl CliReporter {
    /// The stderr line for a notice or a warning; `None` for every other event.
    #[allow(
        dead_code,
        reason = "first caller: D.3b's target arms and D.3c's doctor"
    )]
    pub(crate) fn line(event: &Event) -> Option<String> {
        match event {
            Event::Notice { message } => Some(message.clone()),
            Event::Warning { message } => Some(format!("warning: {message}")),
            _ => None,
        }
    }
}

impl Reporter for CliReporter {
    fn report(&self, event: Event) {
        match &event {
            Event::Output {
                stream: Stream::Stdout,
                bytes,
            } => {
                let _ = std::io::stdout().write_all(bytes);
            }
            Event::Output {
                stream: Stream::Stderr,
                bytes,
            } => {
                let _ = std::io::stderr().write_all(bytes);
            }
            other => {
                if let Some(l) = Self::line(other) {
                    eprintln!("{l}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::Event;

    #[test]
    fn notices_print_verbatim_and_warnings_with_the_prefix() {
        assert_eq!(
            CliReporter::line(&Event::Notice {
                message: "waiting…".into()
            })
            .as_deref(),
            Some("waiting…")
        );
        assert_eq!(
            CliReporter::line(&Event::Warning {
                message: "x".into()
            })
            .as_deref(),
            Some("warning: x")
        );
        assert_eq!(
            CliReporter::line(&Event::Stage {
                index: 1,
                total: 2,
                title: "t".into()
            }),
            None
        );
    }
}
