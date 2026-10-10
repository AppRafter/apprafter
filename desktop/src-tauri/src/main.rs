// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// No console window behind the app in a Windows release build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(e) = apprafter_desktop::run() {
        eprintln!("AppRafter Desktop could not start: {e}");
        std::process::exit(apprafter_desktop::exit_code(&*e));
    }
}
