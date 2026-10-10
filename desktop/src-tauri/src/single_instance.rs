// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The single-instance lock (`tauri-plugin-single-instance`): a second launch brings the running
//! app's window to the front ([`window::show_and_focus`]) and exits, so one app runs per data
//! directory ([`instance_identifier`](crate::env::instance_identifier)).
//!
//! On Linux the plugin keeps the lock as a name on the session bus, and a session without a
//! usable bus can stop the app from starting at all. The plugin's 2.5.2 panics while the app is
//! built when the session bus address does not parse: `DBUS_SESSION_BUS_ADDRESS` set but empty,
//! or set to anything that is no D-Bus address. It unwraps zbus's `Builder::session()`
//! (`src/platform_impl/linux.rs`, line 57). It also connects on the main thread with no bound,
//! so a bus that takes the connection and never answers holds the start for ever. So on Linux
//! the app first connects to the session bus itself, as the plugin is about to, and waits at
//! most [`PROBE_TIMEOUT`] ([`SingleInstance::for_this_launch`]). It registers the plugin only
//! when that connection works ([`SingleInstance::register`]). When it does not, the app starts
//! without the lock, so launching it again starts a second app rather than bringing this one to
//! the front, and the log says so and why ([`SingleInstance::log`]). On a bus that answers, the
//! plugin runs exactly as it did before the check. The check reads no environment of its own:
//! zbus finds the address as the plugin does (`DBUS_SESSION_BUS_ADDRESS`, else
//! `$XDG_RUNTIME_DIR/bus`), and nothing changes it between the two.
//!
//! Why the app starts without a lock rather than with another one, such as a lock file: the
//! lock exists to hand a second launch over to the first app's window, and a lock file cannot
//! do that. It can only refuse the second launch. Handing over would need a channel of its own
//! to the first app, a second single-instance mechanism beside the plugin's, for a session that
//! is broken already. And two apps on one data directory are a nuisance, not a hazard: the
//! settings file is always written whole (an atomic replace, the last save wins), the page
//! keeps nothing in the webview's storage (`src/no-webview-storage.test.ts`), and the target
//! store is the one the CLI works on beside the app at any time.
//!
//! The check and the plugin connect one after the other, so a bus that dies between the two
//! leaves the plugin without its name. It still does not panic, since the address parsed, and
//! the app runs without the lock, unlogged. Only an address that does not parse panics.
//!
//! Upstream (tauri-apps/plugins-workspace), #3542 proposes hardening the Linux backend's error
//! handling, its unwraps included. The panic itself is still on the default branch. The check
//! stays until a release of the plugin neither panics on such an address nor waits without a
//! bound.

#[cfg(target_os = "linux")]
use std::fmt;
#[cfg(target_os = "linux")]
use std::time::Duration;

use tauri::Runtime;

use crate::window;

/// How long the check waits for the session bus to answer: a local bus answers in
/// milliseconds, and a bus that never answers delays the window by no more than this.
#[cfg(target_os = "linux")]
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether this launch keeps the single-instance lock.
#[derive(Debug)]
pub enum SingleInstance {
    /// The plugin is registered: a second launch brings this app's window to the front and
    /// exits.
    On,
    /// Linux only: the session bus cannot keep the lock (why), so the plugin is left out and
    /// the app starts without it.
    #[cfg(target_os = "linux")]
    Off(NoSessionBus),
}

/// Why the session bus cannot keep the single-instance lock. zbus's errors are kept as the text
/// the log shows: nothing reads more of them.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub enum NoSessionBus {
    /// The address does not parse (zbus's error). This is the address the plugin panics on.
    NoAddress(String),
    /// The bus at `address` refused the connection, or is not there (zbus's error).
    Unreachable { address: String, error: String },
    /// The bus at `address` did not answer `within` that long.
    NoAnswer { address: String, within: Duration },
}

#[cfg(target_os = "linux")]
impl fmt::Display for NoSessionBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAddress(error) => write!(
                f,
                "the session bus address (DBUS_SESSION_BUS_ADDRESS, or $XDG_RUNTIME_DIR/bus \
                 when that is unset) is no D-Bus address ({error})"
            ),
            Self::Unreachable { address, error } => {
                write!(
                    f,
                    "the session bus at {address} cannot be reached ({error})"
                )
            }
            Self::NoAnswer { address, within } => {
                write!(
                    f,
                    "the session bus at {address} did not answer within {within:?}"
                )
            }
        }
    }
}

impl SingleInstance {
    /// This launch's lock: on Linux, [`on_session_bus`](Self::on_session_bus) on the address
    /// zbus finds, checked by a [`connect`] bounded by [`PROBE_TIMEOUT`]. Blocks for up to that
    /// long on a bus that never answers.
    #[cfg(target_os = "linux")]
    pub fn for_this_launch() -> Self {
        Self::on_session_bus(zbus::Address::session(), |address| {
            connect(address, PROBE_TIMEOUT)
        })
    }

    /// This launch's lock: always on. Only the Linux plugin needs a bus.
    #[cfg(not(target_os = "linux"))]
    pub fn for_this_launch() -> Self {
        Self::On
    }

    /// The decision, on the session bus `address` as zbus parsed it and on what `connect` makes
    /// of it: off when the address does not parse (`connect` is not called) or `connect`
    /// fails, on otherwise.
    #[cfg(target_os = "linux")]
    pub fn on_session_bus(
        address: zbus::Result<zbus::Address>,
        connect: impl FnOnce(zbus::Address) -> Result<(), NoSessionBus>,
    ) -> Self {
        let address = address.map_err(|error| NoSessionBus::NoAddress(error.to_string()));
        match address.and_then(connect) {
            Ok(()) => Self::On,
            Err(why) => Self::Off(why),
        }
    }

    /// `builder` with the plugin registered when the lock is on, and as it was when it is
    /// off. The app calls this before it registers any other plugin, so a second launch exits
    /// before the others start.
    pub fn register<R: Runtime>(&self, builder: tauri::Builder<R>) -> tauri::Builder<R> {
        match self {
            Self::On => builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
                window::show_and_focus(app)
            })),
            #[cfg(target_os = "linux")]
            Self::Off(_) => builder,
        }
    }

    /// One warning in the log when the lock is off, and why; nothing when it is on. The app
    /// calls this once the log has started, after the app is built.
    pub fn log(&self) {
        match self {
            Self::On => {}
            #[cfg(target_os = "linux")]
            Self::Off(why) => tracing::warn!(
                "single-instance is off, so launching the app again starts a second app rather \
                 than bringing this one to the front: the lock is a name on the session bus, \
                 and {why}"
            ),
        }
    }
}

/// Connect to the bus at `address` as the plugin is about to, then hang up. An error when it
/// refuses, or when it has not answered within `within`. A connection that has not answered by
/// then is dropped.
#[cfg(target_os = "linux")]
pub fn connect(address: zbus::Address, within: Duration) -> Result<(), NoSessionBus> {
    use futures_lite::future;

    let shown = address.to_string();
    let unreachable = |error: zbus::Error| NoSessionBus::Unreachable {
        address: shown.clone(),
        error: error.to_string(),
    };
    let attempt = async {
        let builder = zbus::connection::Builder::address(address).map_err(unreachable)?;
        builder.build().await.map(drop).map_err(unreachable)
    };
    let deadline = async {
        async_io::Timer::after(within).await;
        Err(NoSessionBus::NoAnswer {
            address: shown.clone(),
            within,
        })
    };
    zbus::block_on(future::or(attempt, deadline))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::str::FromStr;

    use super::*;

    /// A `connect` the decision must never call.
    fn never(address: zbus::Address) -> Result<(), NoSessionBus> {
        panic!("connected to {address}")
    }

    #[test]
    fn an_address_that_does_not_parse_is_off_without_connecting() {
        for address in ["", "not-an-address", "unix:"] {
            let decision = SingleInstance::on_session_bus(zbus::Address::from_str(address), never);
            assert!(
                matches!(decision, SingleInstance::Off(NoSessionBus::NoAddress(_))),
                "{address:?}: {decision:?}"
            );
        }
    }

    #[test]
    fn an_address_that_parses_is_on_exactly_when_the_bus_answers() {
        let address = || zbus::Address::from_str("unix:path=/nonexistent/bus");
        let answered = SingleInstance::on_session_bus(address(), |_| Ok(()));
        assert!(matches!(answered, SingleInstance::On), "{answered:?}");

        let silent = SingleInstance::on_session_bus(address(), |address| {
            Err(NoSessionBus::NoAnswer {
                address: address.to_string(),
                within: PROBE_TIMEOUT,
            })
        });
        assert!(
            matches!(silent, SingleInstance::Off(NoSessionBus::NoAnswer { .. })),
            "{silent:?}"
        );
    }

    #[test]
    fn each_reason_names_the_bus_and_what_went_wrong() {
        let error = || zbus::Error::Address("Invalid address".into()).to_string();
        let address = "unix:path=/run/user/1000/bus".to_owned();
        assert_eq!(
            NoSessionBus::NoAddress(error()).to_string(),
            "the session bus address (DBUS_SESSION_BUS_ADDRESS, or $XDG_RUNTIME_DIR/bus when \
             that is unset) is no D-Bus address (address error: Invalid address)"
        );
        assert_eq!(
            NoSessionBus::Unreachable {
                address: address.clone(),
                error: error(),
            }
            .to_string(),
            "the session bus at unix:path=/run/user/1000/bus cannot be reached (address error: \
             Invalid address)"
        );
        assert_eq!(
            NoSessionBus::NoAnswer {
                address,
                within: Duration::from_secs(2),
            }
            .to_string(),
            "the session bus at unix:path=/run/user/1000/bus did not answer within 2s"
        );
    }
}
