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
    "unlock_with_password",
    "activity",
    "quit",
    "op_list",
    "op_subscribe",
    "op_unsubscribe",
    "op_cancel",
    "op_discard",
    "op_execute",
    "window_ready",
    "theme_apply",
    // D.3: targets, doctor, whoami (overview §3.12.1).
    "target_list",
    "target_show",
    "ssh_key_candidates",
    "ssh_key_inspect",
    "toolchain_status",
    "whoami",
    "op_start_verify_token",
    "op_start_machine_catalogue",
    "op_start_region_latencies",
    "op_start_doctor",
    "op_start_whoami",
    "op_plan_target_add",
    "op_plan_target_renew",
    "op_plan_target_use",
    "op_plan_target_rename",
    "op_plan_target_remove",
    "op_plan_target_machine",
    "target_draft_discard",
];

/// What a locked app still answers; everything else is `apprafter::desktop::locked`.
/// `settings_get` stays readable and `theme_apply` applies, so the lock screen renders in the
/// chosen theme, `unlock_with_password` is the lock screen's own password field (where the OS
/// cannot prompt, `AuthInfo.passwordField`), and `window_ready` shows the window, created
/// hidden, once the lock screen has painted.
pub const ALLOWED_WHILE_LOCKED: &[&str] = &[
    "app_info",
    "settings_get",
    "lock_status",
    "unlock",
    "unlock_with_password",
    "quit",
    "window_ready",
    "theme_apply",
];
