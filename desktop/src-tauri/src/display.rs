// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The display the app's window is on (D.4a T72, T73), for the lock's limit rows. On Linux,
//! what GDK opened: an X11 display — any program running as the person can see what is typed
//! into the app there, the lock password included — or a Wayland one, which shows it to no
//! other client. An app GDK put on X11 inside a Wayland session (Xwayland) is an X11 program
//! all the same. Asked once, on the main thread, after the app is built (GTK is up then) and
//! before the event loop; never from a command. macOS and Windows have no such row.

use apprafter_core::lock::limits::DisplayKind;

/// The kind of display GDK's display type names.
pub fn kind_of(gdk_display_type: &str) -> DisplayKind {
    match gdk_display_type {
        "GdkWaylandDisplay" => DisplayKind::Wayland,
        "GdkX11Display" => DisplayKind::X11,
        _ => DisplayKind::None,
    }
}

/// The display the app opened; `None` off Linux, or when GDK has none.
#[cfg(target_os = "linux")]
pub fn current() -> DisplayKind {
    use gtk::glib::prelude::ObjectExt;
    match gtk::gdk::Display::default() {
        Some(display) => kind_of(display.type_().name()),
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
        assert_eq!(kind_of("GdkWaylandDisplay"), DisplayKind::Wayland);
        assert_eq!(kind_of("GdkX11Display"), DisplayKind::X11);
        assert_eq!(kind_of("GdkBroadwayDisplay"), DisplayKind::None);
    }
}
