// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What only the CLI can see of this computer's limits (D.4a T72, T73, O1): its terminal's
//! multiplexer and display (by the core's `display_kind`), and the SSH key and agent a
//! kubeconfig fetch uses. Read through `EnvSource` (`ProcessEnv` in production), so the
//! env-read ratchet does not move.

use std::path::PathBuf;

use apprafter_core::lock::limits::{display_kind, LimitClient, SshFacts, TerminalFacts};
use apprafter_core::EnvSource;

/// The multiplexers `lock status` names, by the variable each sets (T72).
const MULTIPLEXERS: [(&str, &str); 5] = [
    ("TMUX", "tmux"),
    ("STY", "screen"),
    ("ZELLIJ", "zellij"),
    ("WEZTERM_PANE", "wezterm"),
    ("KITTY_LISTEN_ON", "kitty"),
];

/// The CLI's `LimitClient`: its terminal's facts when it runs at one, and the SSH key and
/// agent the kubeconfig fetch would use (O1).
pub(crate) fn cli_limit_client(env: &dyn EnvSource, terminal: bool) -> LimitClient {
    let set = |k: &str| env.var(k).is_some_and(|v| !v.is_empty());
    let terminal = terminal.then(|| TerminalFacts {
        multiplexer: MULTIPLEXERS
            .iter()
            .find(|(var, _)| set(var))
            .map(|(_, name)| (*name).to_string()),
        x_client: Some(env.var("WINDOWID").is_some_and(|v| is_x_window_id(&v))),
        display: display_kind(
            env.var("XDG_SESSION_TYPE").as_deref(),
            set("WAYLAND_DISPLAY"),
            set("DISPLAY"),
        ),
    });
    LimitClient::Cli {
        terminal,
        ssh: SshFacts {
            key: Some(cli_providers::hetzner_cloud::default_ssh_identity_path()),
            agent: ssh_agent_path(env),
        },
    }
}

/// F14: `WINDOWID` holds a resource id an X server could have issued: decimal, or `0x` hex, and
/// within the 29 bits the X protocol gives a resource id (its top three bits are always clear).
/// Konsole exports a Qt pointer there on Wayland too, which is no X id: still a heuristic, but
/// one that no longer calls a native Wayland terminal an X client.
fn is_x_window_id(v: &str) -> bool {
    let id = match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => v.parse::<u64>().ok(),
    };
    id.is_some_and(|id| id > 0 && id < 1 << 29)
}

/// The ssh-agent: `SSH_AUTH_SOCK`, else on Windows the OpenSSH agent service's pipe. (G2: this
/// function spells a pipe name; `ENDPOINT_LITERALS` lists it.)
pub(crate) fn ssh_agent_path(env: &dyn EnvSource) -> Option<PathBuf> {
    if let Some(sock) = env.var("SSH_AUTH_SOCK").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(sock));
    }
    cfg!(windows).then(|| PathBuf::from(r"\\.\pipe\openssh-ssh-agent"))
}

#[cfg(test)]
mod tests {
    use apprafter_core::lock::limits::LimitClient;
    use apprafter_core::MapEnv;

    use super::*;

    /// T72: the multiplexer and the X-client hint come from the terminal's environment, an
    /// empty variable counting as unset; without a terminal there are no terminal facts.
    #[test]
    fn the_terminal_facts_come_from_the_environment_and_none_without_a_terminal() {
        let env = MapEnv::new()
            .with("TMUX", "/tmp/tmux-1000/default,1,0")
            .with("WINDOWID", "4194307");
        match cli_limit_client(&env, true) {
            LimitClient::Cli {
                terminal: Some(t), ..
            } => {
                assert_eq!(
                    (t.multiplexer.as_deref(), t.x_client),
                    (Some("tmux"), Some(true))
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            cli_limit_client(&env, false),
            LimitClient::Cli { terminal: None, .. }
        ));
        match cli_limit_client(
            &MapEnv::new().with("TMUX", "").with("STY", "4242.pts-1"),
            true,
        ) {
            LimitClient::Cli {
                terminal: Some(t), ..
            } => {
                assert_eq!(
                    (t.multiplexer.as_deref(), t.x_client),
                    (Some("screen"), Some(false))
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// F14: only a `WINDOWID` an X server could have issued makes the terminal an X client.
    /// X resource ids are 29 bits (the top three bits clear); Konsole on Wayland exports a Qt
    /// pointer there (97144953273280 in the walk), which no X server hands out. A hex id counts
    /// too, so a terminal that writes it that way is never under-warned.
    #[test]
    fn a_windowid_counts_only_when_an_x_server_could_have_issued_it() {
        for (windowid, want) in [
            ("97144953273280", false),
            ("18874380", true),
            ("536870911", true),
            ("536870912", false),
            ("0x1200", true),
            ("0X1200", true),
            ("0x20000000", false),
            ("0", false),
            ("0x0", false),
            ("", false),
            ("x", false),
            ("-5", false),
            ("18874380 ", false),
        ] {
            match cli_limit_client(&MapEnv::new().with("WINDOWID", windowid), true) {
                LimitClient::Cli {
                    terminal: Some(t), ..
                } => assert_eq!(t.x_client, Some(want), "WINDOWID={windowid:?}"),
                other => panic!("{other:?}"),
            }
        }
    }

    /// O1: the agent is `SSH_AUTH_SOCK`, else on Windows the OpenSSH agent service's pipe.
    #[test]
    fn the_agent_is_ssh_auth_sock_or_windows_default_pipe() {
        let env = MapEnv::new().with("SSH_AUTH_SOCK", "/tmp/agent.1");
        assert_eq!(
            ssh_agent_path(&env),
            Some(std::path::PathBuf::from("/tmp/agent.1"))
        );
        let none = MapEnv::new().with("SSH_AUTH_SOCK", "");
        if cfg!(windows) {
            assert_eq!(
                ssh_agent_path(&none),
                Some(std::path::PathBuf::from(r"\\.\pipe\openssh-ssh-agent"))
            );
        } else {
            assert_eq!(ssh_agent_path(&none), None);
        }
    }
}
