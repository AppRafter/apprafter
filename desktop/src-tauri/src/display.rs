// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The display the app's window is on (D.4a T72, T73), for the lock's limit rows. On Linux,
//! what GDK opened: an X11 display — any program running as the person can see what is typed
//! into the app there, the lock password included — or a Wayland one, which shows it to no
//! other client. An app GDK put on X11 inside a Wayland session (`GDK_BACKEND=x11`) runs under
//! Xwayland: an X11 program all the same, but the session is Wayland's, so its row says so
//! (F21). Asked once, on the main thread, after the app is built (GTK is up then) and before
//! the event loop; never from a command. macOS and Windows have no such row.

use apprafter_core::lock::limits::DisplayKind;

/// The kind of display GDK's display type names; an X11 display in a Wayland session
/// (`wayland_session`, [`crate::env::GraphicsFacts::in_wayland_session`]) is Xwayland.
pub fn kind_of(gdk_display_type: &str, wayland_session: bool) -> DisplayKind {
    match gdk_display_type {
        "GdkWaylandDisplay" => DisplayKind::Wayland,
        "GdkX11Display" if wayland_session => DisplayKind::Xwayland,
        "GdkX11Display" => DisplayKind::X11,
        _ => DisplayKind::None,
    }
}

/// The display the app opened; `None` off Linux, or when GDK has none.
#[cfg(target_os = "linux")]
pub fn current() -> DisplayKind {
    use gtk::glib::prelude::ObjectExt;
    let wayland_session = crate::env::GraphicsFacts::from_process().in_wayland_session();
    match gtk::gdk::Display::default() {
        Some(display) => kind_of(display.type_().name(), wayland_session),
        None => DisplayKind::None,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn current() -> DisplayKind {
    DisplayKind::None
}

#[cfg(test)]
mod tests {
    use apprafter_core::lock::limits::DisplayKind;

    use super::kind_of;

    #[test]
    fn gdk_s_display_type_names_the_kind() {
        for session in [false, true] {
            assert_eq!(kind_of("GdkWaylandDisplay", session), DisplayKind::Wayland);
            assert_eq!(kind_of("GdkBroadwayDisplay", session), DisplayKind::None);
        }
        assert_eq!(kind_of("GdkX11Display", false), DisplayKind::X11);
    }

    /// F21: GDK's X11 display inside a Wayland session (`GDK_BACKEND=x11`) is Xwayland, so the
    /// app's row is its own and never "X11 session".
    #[test]
    fn an_x11_display_in_a_wayland_session_is_xwayland() {
        assert_eq!(kind_of("GdkX11Display", true), DisplayKind::Xwayland);
    }
}
