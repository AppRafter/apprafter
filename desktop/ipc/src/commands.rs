// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// One list, three uses: src-tauri/build.rs (the app ACL manifest), the invoke handler's
// registration (checked by src-tauri/tests/ipc_mock.rs) and src/ipc/generated/commands.ts.
// build.rs `include!`s this file at module level (in a function body `include!` takes an
// expression, not items): plain items only — no `//!`, no `use`, no `crate::`/`super::`
// paths. lib.rs's tests include it the same way.

/// Every command the shell registers.
pub const COMMANDS: &[&str] = &[
    "app_info",
    "settings_get",
    "settings_set",
    "lock_status",
    "lock_now",
    "unlock",
    "activity",
    "quit",
    "op_list",
    "op_subscribe",
    "op_unsubscribe",
    "op_cancel",
    "op_discard",
    "op_execute",
    "window_ready",
];

/// What a locked app still answers; everything else is `apprafter::desktop::locked`.
/// `settings_get` stays readable so the lock screen renders in the chosen theme, and
/// `window_ready` shows the window, created hidden, once the lock screen has painted.
pub const ALLOWED_WHILE_LOCKED: &[&str] = &[
    "app_info",
    "settings_get",
    "lock_status",
    "unlock",
    "quit",
    "window_ready",
];
