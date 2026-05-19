// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter auth …` — reserved namespace for AppRafter Cloud
//! (Managed) authentication, per `cli-dx-task.md` §3.1 + §5.7 +
//! §3.4. Today all three subcommands (login / logout / status)
//! print the same friendly redirect to `apprafter target add`
//! plus a pointer to the Managed roadmap.
//!
//! Kept as a real `Subcommand` enum + a hidden top-level command
//! from day one so the future Managed implementation can fill in
//! each handler without reshaping the CLI surface. The
//! `#[command(hide = true)]` annotation on `Commands::Auth`
//! prevents the group from crowding `apprafter --help` for new
//! users; `apprafter auth --help` still works for the
//! intentionally-curious.

use cli_core::Result;
use tracing::info;

use crate::cli::AuthCommand;

const MANAGED_ROADMAP_URL: &str = "https://apprafter.dev";

pub fn run(action: AuthCommand) -> Result<()> {
    match action {
        AuthCommand::Login => run_login(),
        AuthCommand::Logout => run_logout(),
        AuthCommand::Status => run_status(),
    }
}

fn run_login() -> Result<()> {
    info!("auth login (stub) invoked");
    print_redirect("AppRafter Cloud is not yet available.");
    Ok(())
}

fn run_logout() -> Result<()> {
    info!("auth logout (stub) invoked");
    print_redirect("AppRafter Cloud is not yet available — nothing to log out from.");
    Ok(())
}

fn run_status() -> Result<()> {
    info!("auth status (stub) invoked");
    println!("AppRafter Cloud authentication: not available yet.");
    println!();
    println!(
        "Self-hosted mode active. Use `apprafter whoami` for a summary of your current target."
    );
    println!();
    println!("Track managed availability at: {MANAGED_ROADMAP_URL}");
    Ok(())
}

fn print_redirect(headline: &str) {
    println!("{headline}");
    println!();
    println!("For self-hosted use, configure a deployment target instead:");
    println!("    apprafter target add");
    println!();
    println!("Track managed availability at: {MANAGED_ROADMAP_URL}");
}
