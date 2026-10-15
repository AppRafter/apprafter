// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! WI-479: a target store AppRafter makes from nothing is its account's alone from its first
//! folder, whatever the umask. Under a shared group's umask (002: NixOS's `users`) `target add`
//! used to make the store's folders group-writable, and WI-475's check then refused the store
//! (`store_not_private`) — after the wizard had asked for the token, since there was nothing to
//! check before the save. Now `target add` on a fresh store completes, the wizard included,
//! every folder it makes is 0700 (the owner-only DACL on Windows), the folders above the store
//! are made as the umask says, and a store folder already there is never changed: one others
//! can write is refused as before, and left as it was.
//!
//! Each case runs `apprafter` through the spawn point (`common::spawn`) on a store under the
//! sandbox root, `<root>/fresh/parent/store`, whose parents are missing too.

mod common;

use std::path::{Path, PathBuf};

use common::spawn::Spawn;

const TOKEN: &str = "uuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu";

/// `target add u`, flag-driven.
const ADD: [&str; 11] = [
    "target",
    "add",
    "u",
    "--provider",
    "hetzner-cloud",
    "--token",
    TOKEN,
    "--region",
    "nbg1",
    "--no-ping",
    "--no-interactive",
];

/// The folders `target add` makes in a fresh store, by path under its root (the root `""`).
const MADE: [&str; 4] = ["", "auth", "targets", "targets/u"];

/// `<root>/fresh/parent/store`, with nothing of it there yet.
fn fresh_store(sp: &Spawn) -> PathBuf {
    assert!(!sp.path("fresh").exists());
    sp.path("fresh").join("parent").join("store")
}

/// The folders the store's root sits in that `target add` made: `fresh/parent` and `fresh`.
fn made_above(sp: &Spawn) -> [PathBuf; 2] {
    [sp.path("fresh"), sp.path("fresh").join("parent")]
}

/// Every folder under `root` and `root` itself (`""`), `/`-separated, sorted.
fn folders_under(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let rel = dir.strip_prefix(root).unwrap();
        out.push(
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/"),
        );
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &entry.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Under a shared group's umask (002) and under none (000): `target add` on a fresh store
/// succeeds, every folder it makes in the store is 0700, the folders above it have the
/// umask's mode, and the store is used again (`target use` takes the lock and its check).
#[cfg(unix)]
#[test]
fn target_add_makes_a_fresh_store_0700_whatever_the_umask() {
    for mask in [0o002, 0o000] {
        let sp = Spawn::new().with_umask(mask);
        let store = fresh_store(&sp);
        let sp = sp.with_env("APPRAFTER_CONFIG_DIR", &store);
        let out = sp.cmd(&ADD).output().unwrap();
        let said = text(&out);
        assert!(out.status.success(), "umask {mask:03o}:\n{said}");
        assert!(!said.contains("store_not_private"), "{said}");
        assert_eq!(folders_under(&store), MADE, "umask {mask:03o}");
        for rel in MADE {
            let folder = store.join(rel);
            assert_eq!(mode(&folder), 0o700, "{folder:?}, umask {mask:03o}");
        }
        for above in made_above(&sp) {
            assert_eq!(mode(&above), 0o777 & !mask, "{above:?}, umask {mask:03o}");
        }
        let out = sp.cmd(&["target", "use", "u"]).output().unwrap();
        assert!(out.status.success(), "umask {mask:03o}:\n{}", text(&out));
    }
}

/// The `target add` wizard at a terminal, on a fresh store under umask 002: it asks for the
/// token (a well-formed fake, typed at the masked prompt; `--no-ping`, so nothing is sent),
/// then offers the account's SSH key (Enter takes it), saves, and is never refused — the docs'
/// "refused before it asks" holds, since a store it makes passes the check. Every folder it
/// made is 0700.
#[cfg(unix)]
#[test]
fn the_target_add_wizard_completes_on_a_fresh_store_under_umask_002() {
    use common::terminal::{converse_at_terminal, line, Step};
    let sp = Spawn::new().with_umask(0o002);
    std::fs::create_dir_all(sp.home().join(".ssh")).unwrap();
    std::fs::write(
        sp.home().join(".ssh").join("id_ed25519.pub"),
        "ssh-ed25519 AAAA test@host\n",
    )
    .unwrap();
    let store = fresh_store(&sp);
    let sp = sp.with_env("APPRAFTER_CONFIG_DIR", &store);
    let args = [
        "target",
        "add",
        "u",
        "--provider",
        "hetzner-cloud",
        "--region",
        "nbg1",
        "--tier",
        "solo",
        "--no-ping",
    ];
    let end = converse_at_terminal(
        &sp,
        &args,
        &[
            Step::Expect("Hetzner Cloud API token:"),
            line(TOKEN),
            Step::Expect("SSH public key:"),
            line(""),
            Step::Expect("target `u` saved and set as active"),
        ],
    );
    let shown = &end.terminal;
    assert_eq!(end.status.code(), Some(0), "{shown}");
    assert!(!shown.contains("store_not_private"), "{shown}");
    assert!(!shown.contains(TOKEN), "the token was shown:\n{shown}");
    assert!(end.restored, "the terminal was not put back");
    assert_eq!(folders_under(&store), MADE);
    for rel in MADE {
        let folder = store.join(rel);
        assert_eq!(mode(&folder), 0o700, "{folder:?}");
    }
    for above in made_above(&sp) {
        assert_eq!(mode(&above), 0o775, "{above:?}");
    }
}

/// A store whose `targets` other accounts can write, made before (by hand, or by an older
/// release under such a umask): `target add` is refused naming it, under umask 002 as under
/// any, makes nothing, and leaves every folder's mode as it was.
#[cfg(unix)]
#[test]
fn a_store_folder_others_can_write_is_refused_and_left_as_it_was() {
    use std::os::unix::fs::PermissionsExt;
    let sp = Spawn::new().with_umask(0o002);
    let store = sp.path("wide").join("store");
    let targets = store.join("targets");
    std::fs::create_dir_all(&targets).unwrap();
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&targets, std::fs::Permissions::from_mode(0o777)).unwrap();
    let sp = sp.with_env("APPRAFTER_CONFIG_DIR", &store);
    let out = sp.cmd(&ADD).output().unwrap();
    let said = text(&out);
    assert_eq!(out.status.code(), Some(1), "{said}");
    let unwrapped: String = said.replace('│', " ").split_whitespace().collect();
    let named: String = format!("other accounts can write to {}", targets.display())
        .split_whitespace()
        .collect();
    assert!(
        said.contains("apprafter::secrets::store_not_private") && unwrapped.contains(&named),
        "{said}"
    );
    assert_eq!(folders_under(&store), ["", "targets"]);
    assert_eq!((mode(&store), mode(&targets)), (0o755, 0o777));
}

/// Windows: `target add` on a fresh store succeeds, every folder it makes in the store has the
/// owner-only DACL (read back), the folders above it inherit theirs, and the store is used
/// again.
#[cfg(windows)]
#[test]
fn target_add_makes_a_fresh_store_owner_only() {
    use cli_core::owner_only::testing;
    let sp = Spawn::new();
    let store = fresh_store(&sp);
    let sp = sp.with_env("APPRAFTER_CONFIG_DIR", &store);
    let out = sp.cmd(&ADD).output().unwrap();
    let said = text(&out);
    assert!(out.status.success(), "{said}");
    assert_eq!(folders_under(&store), MADE);
    for rel in MADE {
        let folder = store.join(rel);
        testing::of_path(&folder)
            .unwrap()
            .assert_owner_only_dir(&folder.display().to_string());
    }
    for above in made_above(&sp) {
        let dacl = testing::of_path(&above).unwrap();
        assert!(!dacl.protected, "{above:?} inherits: {dacl:?}");
    }
    let out = sp.cmd(&["target", "use", "u"]).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
}

/// Windows: a store whose `targets` DACL lets other accounts write is refused, makes nothing,
/// and keeps that DACL.
#[cfg(windows)]
#[test]
fn a_store_folder_others_can_write_is_refused_and_left_as_it_was() {
    use cli_core::owner_only::testing;
    let sp = Spawn::new();
    let store = sp.path("wide").join("store");
    let targets = store.join("targets");
    std::fs::create_dir_all(&targets).unwrap();
    let me = testing::this_user_sid().unwrap();
    testing::set_dacl(
        &targets,
        &format!("D:P(A;OICI;FA;;;{me})(A;OICI;0x1301bf;;;BU)"),
    )
    .unwrap();
    let wide = testing::of_path(&targets).unwrap();
    let sp = sp.with_env("APPRAFTER_CONFIG_DIR", &store);
    let out = sp.cmd(&ADD).output().unwrap();
    let said = text(&out);
    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(
        said.contains("apprafter::secrets::store_not_private"),
        "{said}"
    );
    assert_eq!(folders_under(&store), ["", "targets"]);
    assert_eq!(testing::of_path(&targets).unwrap(), wide);
}
