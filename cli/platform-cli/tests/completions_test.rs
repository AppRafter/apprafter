// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every shell offered emits a script that is real.
//!
//! `apprafter completion <shell>` advertises a fixed set of shells in
//! `--help`, and each entry is a claim: that this binary can produce a
//! working completion script for that shell. Nothing else in the
//! repository judges it. `docsgen check` sees only that the command and
//! its examples resolve against the clap tree, which is true of a
//! command that prints an empty string.
//!
//! So the assertions run the **shipped binary** — the dispatch arm
//! included, because a handler nothing reaches is the failure mode a
//! unit test over the generator would sail past — and compare each
//! script against the clap tree it must describe. Artefact against
//! committed source, never a snapshot against a snapshot.
//!
//! The shell list is read from `clap_complete::Shell::value_variants()`
//! rather than written here. Writing it would put a second copy of the
//! supported set in the one place meant to check the first, so a shell
//! dropped from the argument and from the test in the same edit would
//! still pass.
//!
//! # `--install` is judged by the filesystem it claims to have written
//!
//! The flag's whole claim is that a file now exists where that shell
//! reads completions from, holding the script this binary prints. Both
//! halves are checked against a throwaway `HOME`: the path, because a
//! destination that is merely plausible completes nothing, and the
//! bytes, because a truncated or half-written script is worse than no
//! script at all. The assertions run the shipped binary for the same
//! reason the rest of this file does — a dispatch arm that never
//! reaches the handler is the failure a unit test over the writer
//! would sail past.

use std::fs;
use std::path::Path;

use clap::CommandFactory;
use clap::ValueEnum;
use clap_complete::Shell;

use apprafter::docs_api::Cli;
use assert_cmd::Command;
use tempfile::TempDir;

/// Run the real binary and hand back what it printed.
fn script_for(shell: &str) -> String {
    let output = Command::cargo_bin("apprafter")
        .unwrap()
        .args(["completion", shell])
        .assert()
        .success();
    String::from_utf8(output.get_output().stdout.clone())
        .unwrap_or_else(|_| panic!("the {shell} script is not UTF-8"))
}

/// The shells the argument accepts, spelled as a user types them.
fn offered_shells() -> Vec<String> {
    Shell::value_variants()
        .iter()
        .map(|shell| shell.to_string())
        .collect()
}

/// Top-level commands `--help` lists. Hidden ones are excluded on
/// purpose: this is a superset assertion, and the generator does emit
/// hidden subcommands (see `commands::completions`), so demanding their
/// absence here would pin somebody else's behaviour rather than ours.
fn visible_top_level_commands() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(|command| command.get_name().to_string())
        .collect()
}

#[test]
fn every_offered_shell_emits_a_script_that_describes_this_cli() {
    let shells = offered_shells();
    let commands = visible_top_level_commands();
    // Both sides are derived, so both can go empty — and "every one of
    // nothing satisfies everything" is how a check of this shape dies
    // quietly.
    assert!(shells.len() >= 3, "only {shells:?} offered");
    assert!(commands.len() >= 20, "only {commands:?} in the clap tree");

    for shell in &shells {
        let script = script_for(shell);
        assert!(
            script.lines().count() > 50,
            "the {shell} script is {} line(s) — that is not a completion \
             script",
            script.lines().count()
        );
        for command in &commands {
            assert!(
                script.contains(command.as_str()),
                "the {shell} script never mentions `apprafter {command}`, \
                 so it does not complete this CLI"
            );
        }
    }
}

#[test]
fn every_script_completes_the_name_this_binary_answers_to() {
    // The generator writes the binary name into the script, and a
    // script keyed to some other name completes nothing. Read from the
    // clap tree so a rename cannot leave the two disagreeing.
    let binary = Cli::command().get_name().to_string();
    assert!(!binary.is_empty());
    for shell in offered_shells() {
        assert!(
            script_for(&shell).contains(&binary),
            "the {shell} script never names `{binary}`"
        );
    }
}

/// The three shells `--install` has a destination for, each with the
/// path it must land on under a `HOME` with no XDG overrides.
///
/// Written out rather than derived: this is the OTHER SIDE of the
/// assertion. Reading the destination back off the code that computes
/// it would pass on any path that code happens to produce, which is
/// the one thing a reader of this test needs proved.
const INSTALL_DESTINATIONS: &[(&str, &str)] = &[
    ("bash", ".local/share/bash-completion/completions/apprafter"),
    ("zsh", ".zfunc/_apprafter"),
    ("fish", ".config/fish/completions/apprafter.fish"),
];

/// Run the real binary against a throwaway `HOME`.
///
/// `XDG_DATA_HOME` and `XDG_CONFIG_HOME` are cleared unless `env`
/// sets them: either may be set in the environment running the suite,
/// and a test that wrote into somebody's real `~/.config` would be a
/// defect of its own rather than a check.
fn run_in_home(args: &[&str], home: &Path, env: &[(&str, &str)]) -> Command {
    let mut command = Command::cargo_bin("apprafter").unwrap();
    command
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME");
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

/// Install `shell`'s script into `home`, and hand back what the
/// command wrote to stdout and stderr.
fn install(shell: &str, home: &Path, env: &[(&str, &str)]) -> (String, String) {
    let assert = run_in_home(&["completion", shell, "--install"], home, env)
        .assert()
        .success();
    let output = assert.get_output();
    (
        String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8"),
    )
}

/// Every path under `root`, relative to it — used to assert that a
/// command which must write nothing wrote nothing.
fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).expect("the throwaway HOME is readable") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            out.push(
                path.strip_prefix(root)
                    .expect("walked from root")
                    .display()
                    .to_string(),
            );
        }
    }
    out.sort();
    out
}

#[test]
fn install_writes_each_shells_script_where_that_shell_reads_it() {
    let home = TempDir::new().expect("a temp HOME");
    for (shell, relative) in INSTALL_DESTINATIONS {
        let destination = home.path().join(relative);
        assert!(
            !destination.parent().expect("a parent").exists(),
            "{shell}: the directory exists before the command ran, so \
             creating it is not what is being tested"
        );

        install(shell, home.path(), &[]);

        let written = fs::read_to_string(&destination).unwrap_or_else(|error| {
            panic!("{shell}: nothing at {}: {error}", destination.display())
        });
        assert_eq!(
            written,
            script_for(shell),
            "{shell}: the installed file is not the script this binary prints"
        );
    }
}

#[test]
fn install_honours_the_xdg_directories_when_they_are_set() {
    // `~/.local/share` and `~/.config` are where these two land with
    // XDG unset; a reader who moved either has moved the place their
    // shell reads, so the destination has to move with it.
    let home = TempDir::new().expect("a temp HOME");
    let data = TempDir::new().expect("a temp XDG_DATA_HOME");
    let config = TempDir::new().expect("a temp XDG_CONFIG_HOME");

    install(
        "bash",
        home.path(),
        &[("XDG_DATA_HOME", &data.path().display().to_string())],
    );
    assert!(
        data.path()
            .join("bash-completion/completions/apprafter")
            .is_file(),
        "XDG_DATA_HOME was ignored: {:?}",
        tree(data.path())
    );

    install(
        "fish",
        home.path(),
        &[("XDG_CONFIG_HOME", &config.path().display().to_string())],
    );
    assert!(
        config
            .path()
            .join("fish/completions/apprafter.fish")
            .is_file(),
        "XDG_CONFIG_HOME was ignored: {:?}",
        tree(config.path())
    );
}

#[test]
fn install_still_prints_the_script_so_one_line_can_install_and_apply() {
    // `source <(apprafter completion bash --install)` is the documented
    // one-liner, and it works only because stdout still carries the
    // script when something is reading it. Under a test harness stdout
    // is a pipe, which is exactly that case.
    let home = TempDir::new().expect("a temp HOME");
    let (stdout, _) = install("bash", home.path(), &[]);
    assert_eq!(
        stdout,
        script_for("bash"),
        "stdout no longer carries the script, so sourcing the install \
         would define no completions"
    );
}

#[test]
fn the_install_report_goes_to_stderr_and_names_the_destination() {
    // On stderr rather than stdout because stdout is sourced: a report
    // in it would be fed to the shell as code.
    let home = TempDir::new().expect("a temp HOME");
    for (shell, relative) in INSTALL_DESTINATIONS {
        let (_, stderr) = install(shell, home.path(), &[]);
        let destination = home.path().join(relative);
        assert!(
            stderr.contains(&destination.display().to_string()),
            "{shell}: the report does not name {}:\n{stderr}",
            destination.display()
        );
        assert!(
            stderr.contains("source"),
            "{shell}: the report does not say how to use it in this \
             shell:\n{stderr}"
        );
    }
}

#[test]
fn the_zsh_report_names_the_fpath_line_a_new_shell_needs() {
    // Writing `~/.zfunc/_apprafter` is half the job: zsh reads it only
    // if `~/.zfunc` is on `fpath`, and nothing this command can do from
    // outside the shell puts it there.
    let home = TempDir::new().expect("a temp HOME");
    let (_, stderr) = install("zsh", home.path(), &[]);
    assert!(stderr.contains("fpath"), "{stderr}");
    assert!(stderr.contains("compinit"), "{stderr}");
}

#[test]
fn a_shell_with_no_published_destination_is_refused_and_nothing_is_written() {
    // Derived: every offered shell that is not one of the three with a
    // destination. A shell that grows one later stops being tested here
    // by moving into `INSTALL_DESTINATIONS`, not by being forgotten.
    let installable: Vec<&str> = INSTALL_DESTINATIONS.iter().map(|(s, _)| *s).collect();
    let others: Vec<String> = offered_shells()
        .into_iter()
        .filter(|shell| !installable.contains(&shell.as_str()))
        .collect();
    assert!(!others.is_empty(), "nothing left to refuse");

    for shell in others {
        let home = TempDir::new().expect("a temp HOME");
        let assert = run_in_home(&["completion", &shell, "--install"], home.path(), &[])
            .assert()
            .failure();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).expect("UTF-8");
        for named in &installable {
            assert!(
                stderr.contains(named),
                "{shell}: the refusal does not name `{named}` as one that \
                 does install:\n{stderr}"
            );
        }
        assert!(
            tree(home.path()).is_empty(),
            "{shell}: refused and still wrote {:?}",
            tree(home.path())
        );
    }
}

#[test]
fn without_the_flag_nothing_is_written_anywhere() {
    // The default is still "print, install nothing" — the property the
    // flag exists to make optional.
    let home = TempDir::new().expect("a temp HOME");
    run_in_home(&["completion", "bash"], home.path(), &[])
        .assert()
        .success();
    assert!(
        tree(home.path()).is_empty(),
        "printing the script touched the filesystem: {:?}",
        tree(home.path())
    );
}

#[test]
fn installing_twice_replaces_the_script_rather_than_appending_to_it() {
    // Upgrading is the common case: the script goes stale with the
    // binary, and the fix is to re-run this.
    let home = TempDir::new().expect("a temp HOME");
    install("bash", home.path(), &[]);
    install("bash", home.path(), &[]);
    let destination = home
        .path()
        .join(".local/share/bash-completion/completions/apprafter");
    assert_eq!(
        fs::read_to_string(&destination).expect("installed twice"),
        script_for("bash"),
        "the second install did not replace the first"
    );
}

#[test]
fn an_unsupported_shell_is_refused_and_the_real_ones_are_named() {
    // Proves the argument is still a closed set rather than a free
    // string that quietly emits nothing for a typo.
    let output = Command::cargo_bin("apprafter")
        .unwrap()
        .args(["completion", "tcsh"])
        .assert()
        .failure();
    let stderr = String::from_utf8(output.get_output().stderr.clone()).expect("stderr is UTF-8");
    assert!(stderr.contains("tcsh"), "{stderr}");
    for shell in offered_shells() {
        assert!(
            stderr.contains(&shell),
            "the refusal does not name `{shell}`:\n{stderr}"
        );
    }
}
