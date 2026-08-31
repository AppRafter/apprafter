// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Worked examples, one table, two consumers.
//!
//! An example is a **claim about this CLI**: `apprafter backup enable
//! --bucket …` asserts that the path exists and that the flag is that
//! command's. Two things read the claim — the binary, which renders it
//! into `--help` as `after_help`, and `docsgen`, which projects it into
//! `docs/reference/cli/**`. They must not be able to disagree, so there
//! is exactly one array and both read it.
//!
//! # Why a table rather than a delimiter inside `after_help`
//!
//! The alternative was to render the examples into `after_help` and have
//! `docsgen` parse them back out on a stable delimiter. It was rejected:
//!
//! * It would make the guard read a **rendering** of the truth instead
//!   of the truth. `docsgen check` already byte-compares a render
//!   against a render; adding a second check that derives its input from
//!   the same rendering path is the vacuity this track keeps finding.
//! * A parse that finds nothing **passes**. Change the delimiter, wrap
//!   the block, style it, or write an example that happens to contain
//!   the delimiter, and the guard silently judges zero examples while
//!   still reporting success. A guard whose input can quietly become
//!   empty is not a guard. (`docsgen::examples` still refuses an entry
//!   it cannot read, precisely because that failure mode has to be loud
//!   wherever it can occur.)
//! * A free-text convention is what the design spec warned against, and
//!   there is no escaping story for a delimiter that an author writing
//!   an example would ever remember.
//! * It couples the guard to the presentation. With the table, changing
//!   how `--help` groups, indents or colours the examples cannot break
//!   the check — and the check keeps working before the rendering
//!   exists at all, which is the order this subphase needs.
//!
//! [`crate::docs_api`] re-exports this module's surface, so widening it
//! stays a deliberate act rather than a side effect of a refactor.
//!
//! # What lives here, and what does not
//!
//! The array is content, not policy. Whether every leaf command *has*
//! an example, and whether the examples resolve against the clap tree,
//! are asserted elsewhere: the resolution guard is
//! `docsgen::examples::check` (run by `docsgen check`), because the clap
//! tree is what an example must be true of.

/// One command's examples, keyed by its path **without** the binary
/// name — `&["secret", "seal"]` for `apprafter secret seal`. The root
/// binary is the empty slice, the same key `docsgen`'s projection uses,
/// so the two indexes cannot drift apart.
#[derive(Debug)]
pub struct CommandExamples {
    /// The command path this example set belongs to.
    pub path: &'static [&'static str],
    /// One shell line each, written as a reader would type it, opening
    /// with `apprafter`.
    pub lines: &'static [&'static str],
}

/// Every documented example, in command-path order.
///
/// The guard that judges an entry (`docsgen::examples::check`) shipped
/// before this content did, on purpose: `docs/reference/cli/**` is
/// byte-compared rather than resolved, so an unverified example lands
/// in a blind spot between the two documentation gates. Filling this
/// array without the guard would have shipped unverified invocations
/// inside the artefact whose whole purpose is to be true.
///
/// # House rules for an entry
///
/// * **One line, and it must invoke this command.** Two lines in one
///   entry would be tokenised as one command with the second line's
///   flags charged to the first, so the guard rejects it; write two
///   entries. A setup or follow-up command may share the line through
///   `&&` — `mkdir -p …` before a completion redirect, `export
///   KUBECONFIG=…` after the fetch — because a recipe that only works
///   with a step the reader has to infer is not a worked example. The
///   guard reads every segment and still requires one of them to be
///   this command.
/// * **It must work verbatim.** Not "resolve" — work. The guard checks
///   the path and the flag names, which is all it can; it cannot know
///   that `export KUBECONFIG="$(apprafter kubeconfig)"` points kubectl
///   at a YAML document where it wants a path list, or that a redirect
///   into a directory that does not exist yet exits 1. Both shipped
///   here and both were found at a terminal, not by a gate.
/// * **A leaf command carries at least one; a hidden command carries
///   none.** `auth login` / `logout` / `status` are hidden because
///   AppRafter Cloud is not available yet — an example would advertise
///   a command `--help` deliberately does not list.
/// * **Never invent a flag or a value shape.** The guard resolves flag
///   names against the clap tree, but it does not know that `cx22` is a
///   real Hetzner SKU and `cpx99` is not. Values here were checked
///   against the code that parses them — the tier enum, the egress and
///   autoscale validators, `KNOWN_NEEDS`, `construct_repo_url`.
/// * **Show what the flag table cannot**: the shape of a value, the
///   combination that changes behaviour, the thing a reader gets wrong.
///   An entry that only restates a flag's help text is noise and should
///   be deleted rather than kept to round out a count.
/// * **A placeholder is spelled `<like-this>`** — the notation the
///   operator guides already use. A name that looks real (`my-app`)
///   invites a copy-paste that half-works.
/// * **Aliases are not taught here.** The generated reference lists
///   them; an example's job is to show what the command does, not what
///   else it may be called.
pub const EXAMPLES: &[CommandExamples] = &[
    CommandExamples {
        path: &["app", "add"],
        lines: &[
            "apprafter app add  # register the repo in the cwd, from its git origin",
            "apprafter app add https://github.com/<org>/<repo>.git --name <name>",
            "apprafter app add --env prod  # registers the Argo app <name>-prod",
            "apprafter app add --no-interactive --scaffold  # CI: scaffold, then register",
        ],
    },
    CommandExamples {
        path: &["app", "list"],
        lines: &[
            "apprafter app list",
            "apprafter app list --all-projects --all-managed  # drop both filters",
        ],
    },
    CommandExamples {
        path: &["app", "logs"],
        lines: &[
            "apprafter app logs <name> -f --tail 100",
            "apprafter app logs <name> --env prod",
        ],
    },
    CommandExamples {
        path: &["app", "open"],
        lines: &[
            "apprafter app open <name>",
            "apprafter app open <name> --port 3000 --no-browser",
        ],
    },
    CommandExamples {
        path: &["app", "remove"],
        lines: &[
            "apprafter app remove <name> --yes  # removes EVERY environment of <name>",
            "apprafter app remove <name> --env prod --keep-data  # one env, data kept",
        ],
    },
    CommandExamples {
        path: &["app", "rollback"],
        lines: &[
            "apprafter app rollback <name>  # previous image digest, else previous Git revision",
            "apprafter app rollback <name> --to sha256:<64-hex> --yes  # pin to an image digest",
            "apprafter app rollback <name> --to <revision> --yes  # roll back the Git revision",
        ],
    },
    CommandExamples {
        path: &["app", "unpin"],
        lines: &[
            "apprafter app unpin <name>  # resume following the image tag",
            "apprafter app unpin <name> --env prod --yes",
        ],
    },
    CommandExamples {
        path: &["app", "scaffold"],
        lines: &[
            "apprafter app scaffold --runtime bun --name <name> --namespace <ns>",
            "apprafter app scaffold --needs pg --needs redis",
        ],
    },
    CommandExamples {
        path: &["app", "status"],
        lines: &[
            "apprafter app status <name>",
            "apprafter app status <name> --resources",
        ],
    },
    CommandExamples {
        path: &["app", "validate"],
        lines: &[
            "apprafter app validate  # auto-discovers <cwd>/apprafter/Application.cue",
            "apprafter app validate apprafter/Application.cue",
        ],
    },
    CommandExamples {
        path: &["apply"],
        lines: &[
            "apprafter apply",
            "apprafter apply --target <target> --server-type cx22",
        ],
    },
    CommandExamples {
        path: &["argocd-password"],
        lines: &[
            "apprafter argocd-password",
            "apprafter argocd-password --refresh  # after the admin secret is rotated",
        ],
    },
    CommandExamples {
        path: &["backup", "check"],
        lines: &["apprafter backup check --credential-file <dotenv>"],
    },
    CommandExamples {
        path: &["backup", "create"],
        lines: &[
            "apprafter backup create",
            "apprafter backup create --namespace <ns> --select  # only that namespace",
            "apprafter backup create --repo <path> --staging-mode sequential",
        ],
    },
    CommandExamples {
        path: &["backup", "disable"],
        lines: &["apprafter backup disable"],
    },
    CommandExamples {
        path: &["backup", "enable"],
        lines: &[
            "apprafter backup enable --bucket <bucket> --endpoint <host> \
             --credential-file <dotenv> --i-have-saved-credentials",
            "apprafter backup enable --bucket s3:https://<host>/<bucket> \
             --credential <secret> --i-have-saved-credentials",
        ],
    },
    CommandExamples {
        path: &["backup", "list"],
        lines: &[
            "apprafter backup list",
            "apprafter backup list --repo <path>",
        ],
    },
    CommandExamples {
        path: &["backup", "prune"],
        lines: &[
            "apprafter backup prune --credential-file <dotenv>",
            "apprafter backup prune --credential-file <dotenv> --keep-daily 14 --keep-weekly 8",
        ],
    },
    CommandExamples {
        path: &["backup", "status"],
        lines: &["apprafter backup status"],
    },
    CommandExamples {
        path: &["backup", "unlock"],
        lines: &["apprafter backup unlock --credential-file <dotenv>"],
    },
    CommandExamples {
        path: &["bootstrap-all"],
        lines: &[
            "apprafter bootstrap-all --server-type cx22",
            "apprafter bootstrap-all --dry-run",
        ],
    },
    CommandExamples {
        path: &["cluster-bootstrap"],
        lines: &["apprafter cluster-bootstrap  # phase 3 of bootstrap-all, on its own"],
    },
    // The flag list already names the shells; what it cannot show is
    // where the script has to land, and that is both the whole
    // difficulty and different for every shell. One line per shell that
    // has a published recipe, each ending at its own destination.
    CommandExamples {
        path: &["completion"],
        // The `mkdir -p` is not decoration: on a clean machine none of
        // these directories exists, and the redirect alone fails with
        // `No such file or directory` and exit 1 — on the one command
        // whose example IS the instruction. The developer quickstart's
        // three recipes each open with the same line; these are those
        // recipes, one line each.
        lines: &[
            "mkdir -p ~/.local/share/bash-completion/completions && apprafter completion bash > ~/.local/share/bash-completion/completions/apprafter",
            "mkdir -p ~/.zfunc && apprafter completion zsh > ~/.zfunc/_apprafter  # ~/.zfunc must be on fpath",
            "mkdir -p ~/.config/fish/completions && apprafter completion fish > ~/.config/fish/completions/apprafter.fish",
        ],
    },
    CommandExamples {
        path: &["destroy"],
        lines: &[
            "apprafter destroy --yes  # every Hetzner resource tagged apprafter=true",
            "apprafter destroy --target <target> --yes",
        ],
    },
    CommandExamples {
        path: &["doctor"],
        lines: &[
            "apprafter doctor",
            "apprafter doctor --target <target> --no-ping",
        ],
    },
    CommandExamples {
        path: &["export"],
        lines: &[
            "apprafter export --out <dir>",
            "apprafter export --namespace <ns> --select",
        ],
    },
    CommandExamples {
        path: &["import"],
        lines: &["apprafter import --dry-run", "apprafter import --force"],
    },
    CommandExamples {
        path: &["init"],
        lines: &["apprafter init --provider hetzner-cloud --tier solo --region nbg1"],
    },
    CommandExamples {
        path: &["kubeconfig"],
        lines: &[
            "apprafter kubeconfig --refresh",
            // NOT `export KUBECONFIG="$(apprafter kubeconfig)"`. This
            // command prints the kubeconfig DOCUMENT, and `KUBECONFIG`
            // is a colon-separated list of PATHS — kubectl finds no
            // file in the YAML, falls back to `https://localhost:8080`
            // and reports a connection error, so the reader debugs
            // their cluster instead of their shell. The substitution
            // also swallows a failed fetch: `export X="$(cmd)"` exits
            // 0 whatever `cmd` did. Both quickstarts write the file
            // and export its path; so does this.
            "apprafter kubeconfig > /tmp/kc && export KUBECONFIG=/tmp/kc",
        ],
    },
    CommandExamples {
        path: &["login"],
        lines: &["apprafter login"],
    },
    CommandExamples {
        path: &["migration", "approve"],
        lines: &[
            "apprafter migration approve <plan>",
            "apprafter migration approve <plan> --namespace <ns>",
        ],
    },
    CommandExamples {
        path: &["migration", "list"],
        lines: &["apprafter migration list"],
    },
    CommandExamples {
        path: &["migration", "reject"],
        lines: &["apprafter migration reject <plan>"],
    },
    CommandExamples {
        path: &["node", "prep"],
        lines: &["apprafter node prep", "apprafter node prep --yes"],
    },
    CommandExamples {
        path: &["node", "status"],
        lines: &["apprafter node status"],
    },
    CommandExamples {
        path: &["open", "argocd"],
        lines: &[
            "apprafter open argocd",
            "apprafter open argocd --project platform",
        ],
    },
    CommandExamples {
        path: &["plan"],
        lines: &["apprafter plan"],
    },
    CommandExamples {
        path: &["platform", "autoscale", "set"],
        lines: &[
            "apprafter platform autoscale set up-only",
            "apprafter platform autoscale set off",
        ],
    },
    CommandExamples {
        path: &["platform", "autoscale", "show"],
        lines: &["apprafter platform autoscale show"],
    },
    CommandExamples {
        path: &["platform", "egress", "set"],
        lines: &["apprafter platform egress set internal"],
    },
    CommandExamples {
        path: &["platform", "egress", "show"],
        lines: &["apprafter platform egress show"],
    },
    CommandExamples {
        path: &["platform", "env", "set"],
        lines: &["apprafter platform env set prod"],
    },
    CommandExamples {
        path: &["platform", "env", "show"],
        lines: &["apprafter platform env show"],
    },
    CommandExamples {
        path: &["platform", "freeze"],
        lines: &[
            "apprafter platform freeze cilium",
            "apprafter platform freeze cilium --version 1.16.5",
        ],
    },
    CommandExamples {
        path: &["platform", "rescue"],
        lines: &["apprafter platform rescue --yes"],
    },
    CommandExamples {
        path: &["platform", "status"],
        lines: &[
            "apprafter platform status",
            "apprafter platform status --cached",
        ],
    },
    CommandExamples {
        path: &["platform", "unfreeze"],
        lines: &["apprafter platform unfreeze cilium"],
    },
    CommandExamples {
        path: &["platform", "upgrade"],
        lines: &[
            "apprafter platform upgrade  # clear the pin, follow the channel",
            "apprafter platform upgrade --to 0.2.54",
        ],
    },
    CommandExamples {
        path: &["repo", "creds", "add"],
        lines: &[
            "apprafter repo creds add <name> --url-prefix https://github.com/<org>",
            "apprafter repo creds add <name> --url-prefix <prefix> --no-validate",
        ],
    },
    CommandExamples {
        path: &["repo", "creds", "list"],
        lines: &["apprafter repo creds list"],
    },
    CommandExamples {
        path: &["repo", "creds", "remove"],
        lines: &[
            "apprafter repo creds remove <name>",
            "apprafter repo creds remove <name> --force",
        ],
    },
    CommandExamples {
        path: &["repo", "creds", "rotate"],
        lines: &["apprafter repo creds rotate <name>"],
    },
    CommandExamples {
        path: &["repo", "creds", "show"],
        lines: &["apprafter repo creds show <name>"],
    },
    CommandExamples {
        path: &["restore"],
        lines: &[
            "apprafter restore <repo> --credential-file <dotenv>",
            "apprafter restore <repo> --credential-file <dotenv> --data-only",
            "apprafter restore <repo> --credential-file <dotenv> --reprovision \
             --server-type cx22",
        ],
    },
    CommandExamples {
        path: &["secret", "remove"],
        lines: &["apprafter secret remove <name> --namespace <ns> --yes"],
    },
    CommandExamples {
        path: &["secret", "list"],
        lines: &[
            "apprafter secret list",
            "apprafter secret list --namespace <ns>",
        ],
    },
    CommandExamples {
        path: &["secret", "seal"],
        lines: &[
            "apprafter secret seal <name> --from-literal TOKEN=<value> --namespace <ns>",
            "apprafter secret seal <name> --from-literal USER=<user> --from-literal PASS=<pass>",
            "apprafter secret seal <name> --from-literal TOKEN=<value> --stdout",
        ],
    },
    CommandExamples {
        path: &["status"],
        lines: &["apprafter status"],
    },
    CommandExamples {
        path: &["target", "add"],
        lines: &[
            "apprafter target add <name> --provider hetzner-cloud --region nbg1 --tier solo",
            "apprafter target add <name> --ssh-key ~/.ssh/id_ed25519.pub --server-type cx22",
            "apprafter target add <name> --renew --token <token>",
        ],
    },
    CommandExamples {
        path: &["target", "cert", "import"],
        lines: &[
            "apprafter target cert import <name> --cert ./origin.crt --key ./origin.key",
            "apprafter target cert import <name> --cert ./new.crt --key ./new.key --replace",
        ],
    },
    CommandExamples {
        path: &["target", "domain", "add"],
        lines: &["apprafter target domain add apprafter.dev --cert <name>"],
    },
    CommandExamples {
        path: &["target", "domain", "list"],
        lines: &["apprafter target domain list"],
    },
    CommandExamples {
        path: &["target", "domain", "remove"],
        lines: &[
            "apprafter target domain remove apprafter.dev",
            "apprafter target domain remove apprafter.dev --force",
        ],
    },
    CommandExamples {
        path: &["target", "firewall", "cloudflare-origin"],
        lines: &[
            "apprafter target firewall cloudflare-origin enable",
            "apprafter target firewall cloudflare-origin disable",
        ],
    },
    CommandExamples {
        path: &["target", "ip"],
        lines: &["apprafter target ip"],
    },
    CommandExamples {
        path: &["target", "list"],
        lines: &["apprafter target list"],
    },
    CommandExamples {
        path: &["target", "machine"],
        lines: &[
            "apprafter target machine  # picker over the live Hetzner catalogue",
            "apprafter target machine --server-type cx32 --target <target>",
            "apprafter target machine --server-type cx32 --no-ping",
        ],
    },
    CommandExamples {
        path: &["target", "remove"],
        lines: &["apprafter target remove <name> --yes"],
    },
    CommandExamples {
        path: &["target", "rename"],
        lines: &["apprafter target rename <from> <to>"],
    },
    CommandExamples {
        path: &["target", "show"],
        lines: &["apprafter target show", "apprafter target show <name>"],
    },
    CommandExamples {
        path: &["target", "use"],
        lines: &["apprafter target use <name>"],
    },
    CommandExamples {
        path: &["upgrade-tier"],
        lines: &["apprafter upgrade-tier --to team"],
    },
    CommandExamples {
        path: &["volume", "create"],
        lines: &["apprafter volume create <name> --size 2Gi --namespace <ns>"],
    },
    CommandExamples {
        path: &["volume", "list"],
        lines: &[
            "apprafter volume list  # cluster-wide",
            "apprafter volume list --namespace <ns>",
        ],
    },
    CommandExamples {
        path: &["volume", "rm"],
        lines: &["apprafter volume rm <name> --namespace <ns> --yes"],
    },
    CommandExamples {
        path: &["volume", "status"],
        lines: &["apprafter volume status <name> --namespace <ns>"],
    },
    CommandExamples {
        path: &["whoami"],
        lines: &["apprafter whoami", "apprafter whoami --no-ping"],
    },
];

/// The examples declared for `path`, or an empty slice.
pub fn examples_for(path: &[&str]) -> &'static [&'static str] {
    lookup(EXAMPLES, path)
}

/// The `Examples:` block `--help` shows below the flag list, or `None`
/// when the command declares no example.
///
/// `Examples:` matches clap's own section headings (`Usage:`,
/// `Arguments:`, `Options:`) and the two-space indent matches the
/// column clap starts an argument on, so the block reads as one more
/// section rather than an appendix.
fn after_help(path: &[&str]) -> Option<String> {
    let lines = examples_for(path);
    if lines.is_empty() {
        return None;
    }
    let mut block = String::from("Examples:");
    for line in lines {
        block.push_str("\n  ");
        block.push_str(line);
    }
    Some(block)
}

/// Hang every declared example off its command as `after_help`.
///
/// Applied to [`crate::cli::Cli::command()`] before matching, so the
/// binary and `docsgen` read the SAME array: one renders it into
/// `--help`, the other into `docs/reference/cli/**`. Writing the block
/// into a `#[command(after_help = …)]` attribute per variant instead
/// would put the text in a second place, which is the drift this table
/// exists to prevent.
///
/// The walk keys on the same path `docsgen`'s projection uses — the
/// subcommand names from the root down — so a command that gains an
/// example in the reference gains it in `--help` with no second edit.
///
/// # It refuses rather than overwrites
///
/// A command that already declares an after-help of its own — through
/// `#[command(after_help = …)]` or `after_long_help` — is a **conflict
/// and panics**, because either resolution is silent. Overwriting
/// loses the author's text; leaving it loses the examples. And
/// `after_long_help` in particular does not even collide visibly: clap
/// falls back from it to `after_help`, so declaring one makes `--help`
/// show the attribute and `-h` show the examples. That was measured,
/// not reasoned about — with `after_long_help` on one variant, `--help`
/// dropped its `Examples:` section while `docsgen check` and every
/// example test stayed green.
///
/// The panic is deliberate. This is a property of a *static* tree, so
/// it cannot depend on user input: it either holds for every invocation
/// or for none, and every test that spawns the binary hits it on the
/// first run. `attach_refuses_to_overwrite_a_commands_own_after_help`
/// pins that it fires, and its sibling that it does not.
pub fn attach(command: clap::Command) -> clap::Command {
    attach_at(command, &mut Vec::new())
}

fn attach_at(mut command: clap::Command, path: &mut Vec<String>) -> clap::Command {
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_string())
        .collect();
    for name in names {
        path.push(name.clone());
        command = command.mut_subcommand(name.as_str(), |sub| attach_at(sub, path));
        path.pop();
    }

    let key: Vec<&str> = path.iter().map(String::as_str).collect();
    match after_help(&key) {
        Some(block) => {
            assert!(
                command.get_after_help().is_none() && command.get_after_long_help().is_none(),
                "`apprafter {}` declares its own after-help AND has examples in \
                 `EXAMPLES`. One of them would be lost silently — an \
                 `after_long_help` attribute in particular takes over `--help` \
                 while `-h` keeps showing the examples. Put the text in the \
                 command's doc comment or in the examples table, not both.",
                key.join(" ")
            );
            command.after_help(block)
        }
        None => command,
    }
}

/// The lookup, over an arbitrary table so it can be exercised on one
/// that actually has entries while [`EXAMPLES`] is still empty.
fn lookup(table: &'static [CommandExamples], path: &[&str]) -> &'static [&'static str] {
    table
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.lines)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first path declared twice, if any.
    ///
    /// A duplicate is not a compile error and [`lookup`] would silently
    /// serve the first entry, so the second author's examples would
    /// vanish from both `--help` and the reference with nothing to
    /// notice. `docsgen`'s `the_projection_carries_exactly_what_the_cli
    /// _declares` catches the same thing from the other side, by
    /// counting lines; this catches it here, where the fix is.
    fn duplicate_path(table: &[CommandExamples]) -> Option<Vec<&'static str>> {
        let mut seen: Vec<&'static [&'static str]> = Vec::new();
        for entry in table {
            if seen.contains(&entry.path) {
                return Some(entry.path.to_vec());
            }
            seen.push(entry.path);
        }
        None
    }

    const SAMPLE: &[CommandExamples] = &[
        CommandExamples {
            path: &["app", "list"],
            lines: &["apprafter app list"],
        },
        CommandExamples {
            path: &["secret", "seal"],
            lines: &[
                "apprafter secret seal db-url --namespace web",
                "apprafter secret seal --help",
            ],
        },
    ];

    const DUPLICATED: &[CommandExamples] = &[
        CommandExamples {
            path: &["app", "list"],
            lines: &["apprafter app list"],
        },
        CommandExamples {
            path: &["app", "list"],
            lines: &["apprafter app list --env prod"],
        },
    ];

    #[test]
    fn a_declared_path_returns_its_lines_and_anything_else_is_empty() {
        assert_eq!(lookup(SAMPLE, &["app", "list"]), &["apprafter app list"]);
        assert_eq!(lookup(SAMPLE, &["secret", "seal"]).len(), 2);
        // A prefix of a declared path is a different command.
        assert!(lookup(SAMPLE, &["app"]).is_empty());
        assert!(lookup(SAMPLE, &["app", "list", "extra"]).is_empty());
        assert!(lookup(SAMPLE, &[]).is_empty());
    }

    #[test]
    fn a_duplicate_path_is_found() {
        // Proves the detector fires — the shipped table is empty today,
        // so asserting only on it would assert nothing.
        assert_eq!(duplicate_path(DUPLICATED), Some(vec!["app", "list"]));
        assert_eq!(duplicate_path(SAMPLE), None);
    }

    #[test]
    fn the_shipped_table_declares_each_path_once() {
        assert_eq!(duplicate_path(EXAMPLES), None);
    }

    /// A two-node stand-in for the real tree. `plan` is a path the
    /// shipped [`EXAMPLES`] declares, so [`attach`] has something to
    /// attach there; `whoami` is another. Built by hand rather than
    /// from `Cli::command()` because the point is to hand `attach` a
    /// command that ALREADY carries an after-help, which the real tree
    /// (correctly) never does.
    fn tree_with(plan: clap::Command) -> clap::Command {
        clap::Command::new("apprafter").subcommand(plan)
    }

    #[test]
    fn attach_puts_the_examples_on_a_command_that_declares_none_of_its_own() {
        // The "does not fire" half. Without it, a refusal that fired on
        // everything would look identical to a working one.
        let attached = attach(tree_with(clap::Command::new("plan")));
        let plan = attached
            .get_subcommands()
            .find(|sub| sub.get_name() == "plan")
            .expect("the subcommand survives the walk");
        let after = plan
            .get_after_help()
            .expect("`apprafter plan` has examples, so a block was attached")
            .to_string();
        assert!(after.starts_with("Examples:"), "{after}");
        for line in examples_for(&["plan"]) {
            assert!(after.contains(line), "{after}");
        }
    }

    #[test]
    #[should_panic(expected = "declares its own after-help")]
    fn attach_refuses_a_commands_own_after_help_rather_than_overwriting_it() {
        attach(tree_with(
            clap::Command::new("plan").after_help("something hand-written"),
        ));
    }

    #[test]
    #[should_panic(expected = "declares its own after-help")]
    fn attach_refuses_an_after_long_help_too() {
        // The one that does not collide visibly: clap falls back from
        // `after_long_help` to `after_help`, so this shape makes
        // `--help` drop the examples while `-h` keeps them.
        attach(tree_with(
            clap::Command::new("plan").after_long_help("something hand-written"),
        ));
    }
}
