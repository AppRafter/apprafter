// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! AppRafter Desktop (ADR 0067): the GUI twin of the apprafter CLI over `apprafter-core`.

pub mod auth;
pub mod errors;
pub mod ops;
pub mod window;

/// Build and run the app; returns when the last window closes.
pub fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .setup(|app| {
            window::build_main(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
}
