// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The desktop's colour scheme from the XDG desktop portal (`org.freedesktop.portal.Settings`),
//! on a thread of its own: read at start, read again whenever the portal's name gains an owner,
//! and followed through its `SettingChanged` signal.
//!
//! The watch subscribes to the signal and to the name's owner before it reads, so no change
//! falls between the two, and reports the read's answer first, then every change of
//! `org.freedesktop.appearance` `color-scheme`. A portal that starts after the app (the bus may
//! start it on the first read, more slowly than that read may take) or restarts is read as it
//! takes the name, so a desktop that is dark already is followed from then on, not only from
//! its next switch. The watch reads with `ReadOne`, and with `Read`, the method `ReadOne`
//! replaced, from a portal older than 1.17.1, which answers `ReadOne` with `UnknownMethod`.
//!
//! Every call to the bus is bounded by a timeout. A portal that does not answer in time counts
//! as none until it signals a change or its name gains a new owner; one that keeps its name but
//! never answers stays none. Nothing waits for the thread, and nothing it does blocks the app:
//! it ends when the bus goes away.
//!
//! Who can change the System theme through the watch: the connection that owns the portal's
//! name, and no other. The bus filters the signal by its first two arguments, the namespace and
//! the key, and delivers a broadcast of it only from that connection. A signal sent to the
//! app's connection alone the bus delivers whoever sends it, and zbus (5.19) drops it unless
//! its sender is the connection zbus takes for the owner: asked of the bus at the subscription,
//! then followed through `NameOwnerChanged`. zbus takes that signal from the bus alone — its
//! rule names the sender `org.freedesktop.DBus`, which `zbus_names` (4.3) parses as a unique
//! name, the bus's own, so zbus compares it with the sender the bus writes on every message,
//! which no program can set — and so does the watch's own subscription to the owner. A
//! program that sends the app's connection a `NameOwnerChanged` naming itself the portal's
//! owner, then its own `SettingChanged`, is therefore not heard, and the watch does not read
//! again for it (`tests/theme_portal.rs` sends it all). This rests on that parse: the test
//! fails if a zbus update drops it.

use std::io;
use std::pin::pin;
use std::thread;
use std::time::Duration;

use futures_lite::StreamExt;
use zbus::names::UniqueName;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{connection, proxy, Connection, Message, Proxy};

/// The portal's bus name, object and interface.
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
/// The setting: its namespace and its key.
const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";
/// The bus's answer to a method the object does not have.
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";

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
/// or no answer within `call_timeout` — then every change, and the portal's answer each time its
/// name gains an owner. An `Err` means no thread.
pub fn watch(
    bus: Bus,
    call_timeout: Duration,
    report: impl Fn(Option<u32>) + Send + 'static,
) -> io::Result<()> {
    thread::Builder::new()
        .name("appearance-portal".into())
        .spawn(move || {
            if let Err(e) = zbus::block_on(listen(bus, call_timeout, &report)) {
                tracing::info!(
                    "the desktop's colour scheme cannot be followed ({e}): the System theme \
                     follows GTK's preference"
                );
                report(None);
            }
        })
        .map(drop)
}

/// What the watch hears after its first read.
enum Heard {
    /// A `SettingChanged` signal, from the connection zbus takes for the portal.
    Changed(Message),
    /// The portal's name has a new owner, or none.
    Owner(Option<UniqueName<'static>>),
}

/// Connect, subscribe, read and report; then report each change, and each read when the
/// portal's name gains an owner, until the bus goes away. An `Err` before the first report:
/// nothing was reported.
async fn listen(
    bus: Bus,
    call_timeout: Duration,
    report: &impl Fn(Option<u32>),
) -> zbus::Result<()> {
    let builder = match bus {
        Bus::Session => connection::Builder::session()?,
        Bus::Address(address) => connection::Builder::address(address.as_str())?,
    };
    let connection = builder.method_timeout(call_timeout).build().await?;
    let settings = settings_proxy(&connection).await?;
    let changes = settings
        .receive_signal_with_args("SettingChanged", &[(0, APPEARANCE), (1, COLOR_SCHEME)])
        .await?
        .map(Heard::Changed);
    let owners = settings.receive_owner_changed().await?.map(Heard::Owner);
    report_read(&settings, report).await;
    let mut heard = pin!(changes.or(owners));
    while let Some(event) = heard.next().await {
        match event {
            Heard::Changed(message) => {
                if let Some(scheme) = changed_color_scheme(&message) {
                    tracing::debug!(color_scheme = scheme, "the desktop's colour scheme changed");
                    report(Some(scheme));
                }
            }
            Heard::Owner(Some(owner)) => {
                tracing::info!(
                    %owner,
                    "the XDG desktop portal started: its colour scheme is read again"
                );
                report_read(&settings, report).await;
            }
            Heard::Owner(None) => tracing::info!(
                "the XDG desktop portal stopped: the colour scheme it said last stays until one \
                 starts"
            ),
        }
    }
    tracing::info!("the session bus went away: the desktop's colour scheme is no longer followed");
    Ok(())
}

/// Read the colour scheme, log the answer and report it.
async fn report_read(settings: &Proxy<'_>, report: &impl Fn(Option<u32>)) {
    let scheme = read(settings).await;
    tracing::info!(color_scheme = ?scheme, "the XDG desktop portal's colour scheme");
    report(scheme);
}

/// The colour scheme as the portal says it now, through `ReadOne`, or through `Read` from a
/// portal that does not know `ReadOne`; `None`, with why in the log when the portal did not
/// answer, otherwise.
async fn read(settings: &Proxy<'_>) -> Option<u32> {
    let args = &(APPEARANCE, COLOR_SCHEME);
    let answer = match settings.call::<_, _, OwnedValue>("ReadOne", args).await {
        Err(e) if is_unknown_method(&e) => {
            tracing::info!("the XDG desktop portal has no ReadOne: reading with Read");
            settings.call::<_, _, OwnedValue>("Read", args).await
        }
        answer => answer,
    };
    match answer {
        Ok(value) => color_scheme(&value),
        Err(e) => {
            tracing::info!("the XDG desktop portal did not say the colour scheme: {e}");
            None
        }
    }
}

/// Whether `error` is the bus's answer to a method the object does not have.
fn is_unknown_method(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::MethodError(name, ..) if name.as_str() == UNKNOWN_METHOD)
}

/// The portal's settings, without the property cache zbus would otherwise fill with a call.
async fn settings_proxy(connection: &Connection) -> zbus::Result<Proxy<'static>> {
    proxy::Builder::new(connection)
        .destination(PORTAL)?
        .path(PORTAL_PATH)?
        .interface(SETTINGS)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
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

/// The number in `value`, inside however many variants it comes in (`Read` wraps it in one more
/// than `ReadOne`).
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
