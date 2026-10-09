// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The desktop's colour scheme from the XDG desktop portal (`org.freedesktop.portal.Settings`):
//! read once, then followed through its `SettingChanged` signal, on a thread of its own.
//!
//! The watch subscribes before it reads, so no change falls between the two, and reports the
//! read's answer first, then every change of `org.freedesktop.appearance` `color-scheme`. Every
//! call to the bus is bounded by a timeout: a portal that does not answer counts as none, and
//! the watch goes on listening, so a portal that comes later is still heard. Nothing waits for
//! the thread, and nothing it does blocks the app: it ends when the bus goes away.
//!
//! The bus filters the signal by its first two arguments, the namespace and the key. zbus keeps
//! only signals whose sender is the connection that owns the portal's name, following that name
//! to a new owner, so no other program on the bus can set the app's theme by sending the
//! portal's signal itself.

use std::io;
use std::thread;
use std::time::Duration;

use zbus::blocking::{connection, proxy, Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};
use zbus::Message;

/// The portal's bus name, object and interface.
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
/// The setting: its namespace and its key.
const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";

/// How long any call to the bus may take before the watch gives up on its answer.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// The bus the portal is on.
#[derive(Debug, Clone)]
pub enum Bus {
    /// The session's, as `DBUS_SESSION_BUS_ADDRESS` names it: the app's.
    Session,
    /// The bus at this address: the tests' own.
    Address(String),
}

/// Watch the portal's colour scheme on `bus`, on a thread of its own: `report` gets the first
/// answer — `None` when there is no bus, no portal, no such setting, a value that is no number,
/// or no answer within `call_timeout` — and then every change. An `Err` means no thread.
pub fn watch(
    bus: Bus,
    call_timeout: Duration,
    report: impl Fn(Option<u32>) + Send + 'static,
) -> io::Result<()> {
    thread::Builder::new()
        .name("appearance-portal".into())
        .spawn(move || {
            if let Err(e) = listen(bus, call_timeout, &report) {
                tracing::info!(
                    "the desktop's colour scheme cannot be followed ({e}): the System theme \
                     follows GTK's preference"
                );
                report(None);
            }
        })
        .map(drop)
}

/// Connect, subscribe, read and report; then report each change until the bus goes away. An
/// `Err` before the first report: nothing was reported.
fn listen(bus: Bus, call_timeout: Duration, report: &impl Fn(Option<u32>)) -> zbus::Result<()> {
    let builder = match bus {
        Bus::Session => connection::Builder::session()?,
        Bus::Address(address) => connection::Builder::address(address.as_str())?,
    };
    let connection = builder.method_timeout(call_timeout).build()?;
    let settings = settings_proxy(&connection)?;
    let changes = settings
        .receive_signal_with_args("SettingChanged", &[(0, APPEARANCE), (1, COLOR_SCHEME)])?;
    let first = match settings.call::<_, _, OwnedValue>("ReadOne", &(APPEARANCE, COLOR_SCHEME)) {
        Ok(value) => color_scheme(&value),
        Err(e) => {
            tracing::info!("the XDG desktop portal did not say the colour scheme: {e}");
            None
        }
    };
    tracing::info!(color_scheme = ?first, "the XDG desktop portal's colour scheme");
    report(first);
    for message in changes {
        if let Some(scheme) = changed_color_scheme(&message) {
            tracing::debug!(color_scheme = scheme, "the desktop's colour scheme changed");
            report(Some(scheme));
        }
    }
    tracing::info!("the session bus went away: the desktop's colour scheme is no longer followed");
    Ok(())
}

/// The portal's settings, without the property cache zbus would otherwise fill with a call.
fn settings_proxy(connection: &Connection) -> zbus::Result<Proxy<'static>> {
    proxy::Builder::new(connection)
        .destination(PORTAL)?
        .path(PORTAL_PATH)?
        .interface(SETTINGS)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
}

/// The colour scheme in a `SettingChanged` signal, when it is the one and a number.
fn changed_color_scheme(message: &Message) -> Option<u32> {
    let body = message.body();
    let (namespace, key, value) = body.deserialize::<(&str, &str, OwnedValue)>().ok()?;
    if (namespace, key) != (APPEARANCE, COLOR_SCHEME) {
        return None;
    }
    color_scheme(&value)
}

/// The number in `value`, inside however many variants it comes in (`Read`, the method
/// `ReadOne` replaced, wraps it in two).
fn color_scheme(value: &Value<'_>) -> Option<u32> {
    match value {
        Value::U32(scheme) => Some(*scheme),
        Value::Value(inner) => color_scheme(inner),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::{Str, Value};

    use super::color_scheme;

    #[test]
    fn the_scheme_is_a_number_inside_any_number_of_variants() {
        assert_eq!(color_scheme(&Value::U32(1)), Some(1));
        assert_eq!(color_scheme(&Value::new(Value::U32(2))), Some(2));
        assert_eq!(
            color_scheme(&Value::new(Value::new(Value::U32(0)))),
            Some(0)
        );
        assert_eq!(color_scheme(&Value::I32(1)), None);
        assert_eq!(color_scheme(&Value::Str(Str::from("prefer-dark"))), None);
    }
}
