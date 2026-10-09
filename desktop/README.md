# AppRafter Desktop

AppRafter Desktop is the graphical twin of the `apprafter` CLI, for Linux, macOS and Windows
([ADR 0067](../docs/adr/0067-desktop-app-and-apprafter-core.md)).

It is not released yet. There is no signed installer and no automatic update: you build the
app from this repository, or on Linux install a test package that CI builds.

What works today:

- The window and its settings.
- The app lock. With **Require unlock** on in Settings, the app locks on **Lock now** and,
  as you choose, when it starts, when the computer sleeps or locks, or after a time without
  use. It unlocks only after the operating system's own sign-in confirms that you are the
  person signed in to this computer.
- If nothing on the computer can confirm you, the lock stays off and a banner says so.

The cluster screens are not connected yet, so the app lists no targets.

## Get the source

Clone the whole repository. `desktop/` does not build on its own: it uses the CLI's crates in
`cli/`, and its build runs `scripts/cue` against the CUE module at the repository root.

```sh
git clone https://github.com/AppRafter/apprafter
cd apprafter
```

The first build downloads Rust crates and npm packages, so it needs network access.

## Build requirements (every OS)

- **Rust** through [rustup](https://rustup.rs). `desktop/rust-toolchain.toml` selects
  Rust 1.98, and rustup installs it on the first build.
- **bun 1.4.x** from [bun.sh](https://bun.sh), or through [mise](https://mise.jdx.dev)
  (`mise install` at the repository root; `mise.toml` pins it). bun 1.3 cannot read
  `desktop/bun.lock`.
- **cue v0.17.1**, which the build runs. `scripts/cue` uses `$CUE` or `$CUE_BIN` when one is
  set, then a `cue` on `PATH` that reports exactly v0.17.1, then `nix run .#cue` (x86_64 Linux
  only), and as a last resort any other `cue` on `PATH`, with only a warning.
  Without Nix, install that version from its
  [release page](https://github.com/cue-lang/cue/releases/tag/v0.17.1). On Linux, with `curl`:

  ```sh
  mkdir -p ~/.local/bin
  curl -fsSL https://github.com/cue-lang/cue/releases/download/v0.17.1/cue_v0.17.1_linux_amd64.tar.gz \
    | tar -xz -C ~/.local/bin cue
  ```

  Make sure `~/.local/bin` is on your `PATH`. If another `cue` comes first, set
  `CUE_BIN=~/.local/bin/cue`.

Always build through `bun run tauri …`. A plain `cargo build` makes a development binary that
expects the Vite dev server on `localhost:1420`.

## Linux

What is tested: CI builds the app on Ubuntu 24.04 (x86_64) and installs its packages into
Debian 13 and Fedora 44 containers. The polkit and PAM sign-in is tested in Debian 13
containers. Builds on Debian, Fedora or Arch, and on other architectures, are not tested.

### System packages

Debian and Ubuntu:

```sh
sudo apt-get install build-essential pkg-config file libwebkit2gtk-4.1-dev librsvg2-dev libpam0g-dev
```

Fedora:

```sh
sudo dnf group install c-development && sudo dnf install pkgconf-pkg-config file webkit2gtk4.1-devel librsvg2-devel pam-devel
```

Arch:

```sh
sudo pacman -S --needed base-devel webkit2gtk-4.1 librsvg pam
```

NixOS: see [NixOS](#nixos).

### Install from a package (Debian, Ubuntu, Fedora)

Build the package from `desktop/`:

```sh
cd desktop
bun install --frozen-lockfile
bun run tauri build --bundles deb    # or --bundles rpm; --bundles deb,rpm builds both
```

The packages are `desktop/target/release/bundle/deb/apprafter-desktop_0.1.0_amd64.deb` and
`desktop/target/release/bundle/rpm/apprafter-desktop-0.1.0-1.x86_64.rpm`. CI builds both on
Ubuntu.

Install one, still in `desktop/`:

```sh
sudo apt install ./target/release/bundle/deb/apprafter-desktop_*_amd64.deb   # Debian, Ubuntu
sudo dnf install ./target/release/bundle/rpm/apprafter-desktop-*.x86_64.rpm  # Fedora
```

dnf warns that it skipped OpenPGP checks for 1 package from repository `@commandline`, its name
for a package file given on the command line. That is expected for a package you built. Start
the app from your applications menu (**AppRafter**) or with `apprafter-desktop`.

Every build has version 0.1.0, so installing a newer build the same way can keep the installed
one: dnf always keeps it, and apt keeps it when the package's metadata, such as its installed
size, has not changed. To update, build again and reinstall:

```sh
sudo apt install --reinstall ./target/release/bundle/deb/apprafter-desktop_*_amd64.deb   # Debian, Ubuntu
sudo dnf reinstall ./target/release/bundle/rpm/apprafter-desktop-*.x86_64.rpm            # Fedora
```

The package installs:

| Path | What it is |
| --- | --- |
| `/usr/bin/apprafter-desktop` | The app. |
| `/usr/share/applications/apprafter-desktop.desktop` | The launcher: **AppRafter** in your applications menu. |
| `/usr/share/icons/hicolor/` | The app's icons. |
| `/usr/share/polkit-1/actions/dev.apprafter.desktop.policy` | The polkit policy, owned by root and readable by everyone. |

It depends on WebKitGTK, GTK and the PAM library, and recommends the polkit daemon (`polkitd`
on Debian and Ubuntu, `polkit` on Fedora), which apt and dnf install by default. polkit reads
the new policy by itself. To check that it did:

```sh
pkaction --action-id dev.apprafter.desktop.unlock --verbose   # the "implicit active" line says auth_self
```

On Linux the package, its binary, launcher and icons are named `apprafter-desktop`, after the
binary. The polkit policy keeps the app's identifier, `dev.apprafter.desktop`.

Build a package on a distribution no newer than the one you install it on: the app needs the
glibc it was built against, or a newer one. Never build packages on NixOS. A binary built there
uses the `/nix/store` loader and does not start on other distributions.

#### Packages from CI

Each run of the `test` workflow that touches `desktop/` builds both packages on Ubuntu 24.04,
in the job **desktop Linux packages (deb, rpm)**. The job installs each package into a fresh
Debian 13 or Fedora 44 container, and checks that `pkaction` shows both actions, that `ldd`
resolves every library, and that the binary loads: it starts far enough to refuse an empty
`APPRAFTER_DESKTOP_DATA_DIR`, before it opens any window. Only if every check passes does it
upload the packages, as the artefact `apprafter-desktop-linux-packages`, kept for 30 days.

Download the artefact from the run's summary page on GitHub (this needs a GitHub account with
access to the repository) and unzip it. It holds `deb/apprafter-desktop_0.1.0_amd64.deb` and
`rpm/apprafter-desktop-0.1.0-1.x86_64.rpm`. Use a run on commit 55ce0d72 or later: older
artefacts hold the package under an earlier name, `app-rafter`. In the directory you unzipped
it into:

```sh
sudo apt install ./deb/apprafter-desktop_0.1.0_amd64.deb     # Debian, Ubuntu
sudo dnf install ./rpm/apprafter-desktop-0.1.0-1.x86_64.rpm  # Fedora
```

These are unsigned test builds, and nothing updates them. Every one has version 0.1.0, so to
update, download a newer artefact and reinstall:

```sh
sudo apt install --reinstall ./deb/apprafter-desktop_0.1.0_amd64.deb   # Debian, Ubuntu
sudo dnf reinstall ./rpm/apprafter-desktop-0.1.0-1.x86_64.rpm          # Fedora
```

They are built against Ubuntu 24.04's glibc (2.39), so they need a distribution at least that
new.

### Other distributions

Build the app without packages, then install the binary and the polkit policy by hand:

```sh
cd desktop
bun install --frozen-lockfile
bun run tauri build --no-bundle
install -Dm755 target/release/apprafter-desktop ~/.local/bin/apprafter-desktop
sudo install -Dm644 packaging/linux/dev.apprafter.desktop.policy \
  /usr/share/polkit-1/actions/dev.apprafter.desktop.policy
```

- Keep `--no-bundle`. Without it the build also makes the deb, the rpm and an AppImage.
- The binary is `desktop/target/release/apprafter-desktop`, unless `CARGO_TARGET_DIR` or a
  Cargo config sets another target directory.
- Copy the policy file; do not symlink it. polkit runs as its own user and cannot read files in
  your home directory. It notices the new file by itself; check it with `pkaction` as above.
- No launcher is installed. Start the app from a terminal with `apprafter-desktop`
  (`~/.local/bin` must be on your `PATH`).
- To update, pull the repository, build again and repeat the install step.

### NixOS

Build and run the app inside the repository's dev shell, `nix develop .#desktop`. It needs
flakes enabled and exists for x86_64-linux only. It provides WebKitGTK, GTK, PAM, `pkg-config`
and cue v0.17.1. It does not provide Rust or bun:

- Rust: install nixpkgs' `rustup`, which patches the toolchains it downloads so they run on
  NixOS.
- bun 1.4.x: the flake's nixpkgs has bun 1.3, which cannot read `bun.lock`. Use the `bun` of a
  newer nixpkgs, or the binary from bun.sh with `programs.nix-ld.enable = true;`.

From the repository root:

```sh
nix develop .#desktop --command bash -c 'cd desktop && bun install --frozen-lockfile && bun run tauri build --no-bundle'
nix develop .#desktop --command ./desktop/target/release/apprafter-desktop
```

Start it through the dev shell, which provides its libraries, GIO modules and GSettings
schemas. Started any other way, it may stop with `error while loading shared libraries`.

#### The polkit policy on NixOS

polkit on NixOS does not read `/usr/share/polkit-1/actions`. Install the policy through your
system configuration instead. Copy `desktop/packaging/linux/dev.apprafter.desktop.policy` next
to your `configuration.nix` (in a flake-based configuration, also `git add` it), then add:

```nix
{ pkgs, ... }:
{
  security.polkit.enable = true;
  environment.systemPackages = [
    (pkgs.writeTextDir "share/polkit-1/actions/dev.apprafter.desktop.policy"
      (builtins.readFile ./dev.apprafter.desktop.policy))
  ];
}
```

Run `sudo nixos-rebuild switch`, which restarts polkit for you. Check with
`pkaction --action-id dev.apprafter.desktop.unlock --verbose`.

To try the policy until the next reboot, without a rebuild:

```sh
sudo install -Dm644 desktop/packaging/linux/dev.apprafter.desktop.policy /run/polkit-1/actions/dev.apprafter.desktop.policy
sudo systemctl reload polkit
```

Without the policy the app still works: its password field is checked through the `login` PAM
service.

### How the app confirms it is you

- With the policy installed, unlocking opens the system's polkit dialog, as long as a polkit
  authentication agent runs in your session. GNOME and KDE Plasma start one. On a window
  manager such as sway, Hyprland or i3, start one yourself, for example `hyprpolkitagent` or
  the one from `polkit-gnome`. With the shipped policy, the dialog asks for your own password,
  not an administrator's, and polkit keeps no grant for later. A polkit rule set by your
  administrator can change either.
- Otherwise the lock screen shows its own **System password** field, which PAM checks: without
  the policy, without polkitd, without an agent, outside an active local session (over SSH, for
  example), or when polkit would let the app through without asking (a polkit rule, or the app
  running as root). PAM uses the first of `common-auth`, `system-auth` and `login` that it
  finds in `/etc/pam.d` or `/usr/lib/pam.d`.
- Neither appears, and the lock stays off, when an administrator's polkit rule refuses the app
  in your active local session, or when the app would show the field but PAM finds no service
  to check it with or no account for the user running the app.
- Run the app as your own user, never with `sudo`: the password field checks the account that
  runs the app.

[Sign-in messages](#sign-in-messages) explains what the lock screen can say.

### Uninstall

Installed from a package:

```sh
sudo apt remove apprafter-desktop    # Debian, Ubuntu
sudo dnf remove apprafter-desktop    # Fedora
```

Installed by hand:

```sh
rm ~/.local/bin/apprafter-desktop
sudo rm /usr/share/polkit-1/actions/dev.apprafter.desktop.policy
```

On NixOS, remove the policy from your configuration and rebuild.

Uninstalling keeps the app's settings and logs. To remove them too:

```sh
rm -r ~/.config/dev.apprafter.desktop ~/.local/share/dev.apprafter.desktop
```

Do not delete `~/.config/apprafter`. It belongs to the `apprafter` CLI, whose target store the
app shares.

### Troubleshooting

- **A blank or white window, flicker, a crash on resize, or
  `Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display`.** These are
  WebKitGTK graphics problems, most often on NVIDIA. Tauri's
  [Linux graphics notes](https://tauri.app/develop/debug/linux-graphics/) give workarounds to
  try in order: the kernel parameter `nvidia_drm.modeset=1` (NVIDIA drivers older than 545),
  then `__NV_DISABLE_EXPLICIT_SYNC=1`, `WEBKIT_DISABLE_DMABUF_RENDERER=1` and
  `WEBKIT_DISABLE_COMPOSITING_MODE=1`. Set a variable for one start, for example
  `WEBKIT_DISABLE_DMABUF_RENDERER=1 apprafter-desktop`. These workarounds have not been tested
  with this app.
- **Only the password field, never the system dialog.** Check that the policy is installed
  (`pkaction`, as above), that polkitd is installed and running, that a polkit agent runs in
  your session, and that you run the app as yourself (not with `sudo`) in a local desktop
  session (not over SSH). If you started the agent while the app showed the password field,
  the dialog comes back the next time the app locks.
- **Nothing in the terminal.** A release build writes its own log lines only to its log file;
  see [Files and logs](#files-and-logs). Only a failure to start or to open the log, a crash,
  and GTK's and WebKitGTK's own messages reach the terminal.
- **Anything else.** Attach that day's log file when you report it.

## macOS

Besides the [build requirements](#build-requirements-every-os), you need macOS 13 or later and
the Xcode Command Line Tools (`xcode-select --install`). Put `cue` v0.17.1 on your `PATH`
(`cue_v0.17.1_darwin_arm64.tar.gz` on Apple silicon, `cue_v0.17.1_darwin_amd64.tar.gz` on
Intel), or set `CUE_BIN`.

```sh
cd desktop
bun install --frozen-lockfile
bun run tauri build --no-bundle
./target/release/apprafter-desktop
```

The app unlocks with Touch ID or your Mac password. CI builds and tests it on macOS, but
builds no installer for it yet.

## Windows

Besides the [build requirements](#build-requirements-every-os), you need:

- Visual Studio Build Tools with the **Desktop development with C++** workload.
- rustup with `x86_64-pc-windows-msvc` as its default host triple, which is rustup-init's
  default on Windows (`winget install --id Rustlang.Rustup`).
- WebView2, which Windows 11 includes (and Windows 10 from version 1803).
- `cue.exe` v0.17.1 (`cue_v0.17.1_windows_amd64.zip`) on your `PATH`. On Windows the build
  cannot run `scripts/cue` and uses the `cue` on `PATH`, so `CUE_BIN` has no effect.

To build and start a release build in Git Bash:

```sh
cd desktop
bun install --frozen-lockfile
bun run tauri build --no-bundle
./target/release/apprafter-desktop.exe
```

The app unlocks with Windows Hello, or with your Windows password when Hello is not set up or
**Prefer biometrics** is off. Hello's prompt needs Windows 11; on older Windows the password
dialog stands in. CI tests the app on Windows and builds it there in Git Bash, as a debug
build (`bun run tauri build --debug --no-bundle`) that it does not start. It builds no
installer for Windows yet.

## Sign-in messages

What the lock screen, and the confirmation of an operation, can say when the system does not
confirm you:

| Message | What it means | What to do |
| --- | --- | --- |
| The system could not show its password prompt. | Linux: no polkit authentication agent runs in your session. | Use the **System password** field that appears, or start an agent. |
| Use your system password here instead. | Linux: polkit refused because the app is not in an active local session, for example over SSH. | Type your password in the **System password** field, which takes over. |
| This computer's settings do not allow AppRafter to ask for your password here. | An administrator's rule refuses the app here: a polkit rule on Linux, a restriction on the account on Windows. This is final. | Ask whoever administers the computer. Trying again does not change it. |
| Your system password has expired. Change it, then try again. | Windows: the password is right, but it has expired or must be changed at the next sign-in. | Change it in Windows, then try again. |
| Too many failed attempts. Try again in 30 s. | After repeated failures the app refuses attempts for a while, and counts down. | Wait for the countdown to end. |
| Too many failed attempts. The system will let you try again later. | The system's own limit holds (an account lockout, Touch ID, PAM), and it does not say for how long. | Wait, or unlock the account in the system. |

## Files and logs

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Settings (`settings.json`) | `~/.config/dev.apprafter.desktop/` | `~/Library/Application Support/dev.apprafter.desktop/` | `%APPDATA%\dev.apprafter.desktop\` |
| Logs | `~/.local/share/dev.apprafter.desktop/logs/` | `~/Library/Logs/dev.apprafter.desktop/` | `%LOCALAPPDATA%\dev.apprafter.desktop\logs\` |

On Linux, `XDG_CONFIG_HOME` and `XDG_DATA_HOME` move these as usual, and the window's web
storage is in `~/.local/share/dev.apprafter.desktop/` too.

`APPRAFTER_DESKTOP_DATA_DIR=<dir>` puts the settings in `<dir>` and the logs in `<dir>/logs`,
and runs the app as a separate instance. The app's own code reads only that variable and
`APPRAFTER_CONFIG_DIR` from its environment (a test build reads two more), so `HCLOUD_TOKEN`,
`KUBECONFIG` and `RUST_LOG` have no effect on it. Variables that the system and the libraries
the app uses read still apply, such as `HOME`, `XDG_CONFIG_HOME` and the graphics ones under
[Troubleshooting](#troubleshooting). See
[Environment variables](../docs/reference/environment.md#apprafter-desktop).

### The log

The app writes one log file per day, `apprafter-desktop.YYYY-MM-DD.log`, dated in UTC, and
keeps the newest 14. A release build writes its log lines only to that file. A development
build (`just desktop-dev`, or any `--debug` build) also prints every line in the terminal.

Two things go to the terminal and not to the log: why the app could not start (for example,
`APPRAFTER_DESKTOP_DATA_DIR` is set but empty), and why it runs without a log file. On Windows
a release build has no console of its own, so they may not show; a debug build started from a
terminal shows them.

To follow today's log on Linux:

```sh
tail -f ~/.local/share/dev.apprafter.desktop/logs/apprafter-desktop.$(date -u +%F).log
```

What the system can confirm you with is one `what can verify the owner here` line, logged at
its first answer, which the app asks for at start, and whenever that changes. Every answer the
system gives to a sign-in request is one `the OS answered` line. For example, at start and then
at an unlock through polkit's dialog:

```
2026-10-09T11:19:00.977514Z  INFO apprafter_desktop::auth_cache: what can verify the owner here available=true method=polkit unavailable=none password_field=false biometrics_choice=false
2026-10-09T11:19:14.042318Z  INFO apprafter_desktop::auth_cache: the OS answered purpose=unlock way=prompt outcome=verified took_ms=3120 method=polkit available=true password_field=false
```

| Field | Values |
| --- | --- |
| `purpose` | `unlock`, or `confirm(<verb>)` for an operation. Never the target's name. |
| `way` | `prompt` (the system's own dialog) or `password_field` (the app's field). |
| `outcome` | `verified`; `cancelled(by=user)`, `cancelled(by=app)` or `cancelled(by=system)`; `failed(exhausted=…)`, with `,retry_in_ms=…` while the app holds further attempts; `busy`; `unavailable(reason=…)`, with the reason in snake_case, such as `policy_missing` or `no_agent`. |
| `took_ms` | How long the system took to answer. |
| `method` | `polkit`, `pam`, `windows_hello`, `windows_credential`, `mac_local_authentication` or `none`. In `the OS answered`, `unanswered` before the system's first answer. |
| `unavailable` | Why nothing can confirm you, in snake_case, or `none`. |
| `available`, `password_field` | Whether anything can confirm you, and whether the app shows its own password field. |
| `messages` | On the password field only: how many messages PAM sent. Never their text. |

The log never contains a password.

The OS session watch, which locks the app when the computer sleeps or locks, logs
`the OS session watch listens listening=Listening { lock: …, sleep: … }` at start, and
`the OS session locked or is going to sleep event=Locked` (or `event=Sleeping`) each time it
hears one. If the system reports neither locks nor sleeps, it logs
`the OS reports neither the session's locks nor sleeps to the app here: lock-on-sleep has nothing to follow`
at start instead, and the app cannot lock when the computer sleeps or locks.

## Development and tests

```sh
just desktop-dev        # the app with hot reload
just desktop-check      # fmt, clippy, cargo test, the lock and IPC-type checks, bun lint and tests
just desktop-build      # a debug build without packages: desktop/target/debug/apprafter-desktop
just desktop-ipc-types  # after changing a type in desktop/ipc; commit the result
```

On Linux, `desktop-dev`, `desktop-build` and `desktop-check` first check the
[system packages](#system-packages) with `pkg-config`. On NixOS, run the recipes inside
`nix develop .#desktop`. On Windows, run `just` from a Git Bash shell: its recipes run with
bash, and its shebang recipes need Git Bash's `cygpath`.

A development build asks the real system for sign-in. A test build,
`cd desktop && bun run tauri dev --features test-build`, uses a scripted stand-in instead (see
`APPRAFTER_DESKTOP_TEST_PASSWORD` in
[Environment variables](../docs/reference/environment.md#apprafter-desktop)).

The interface in a browser, on mocked data, without building any Rust:

```sh
cd desktop
bun install --frozen-lockfile
bun run dev:mock    # http://localhost:1420
```

The browser tests (Playwright) also need Node.js:

```sh
cd desktop
bun install --frozen-lockfile
npx --no-install playwright install --with-deps chromium webkit   # once; drop --with-deps off Debian and Ubuntu
bun run e2e
```

On NixOS, the browsers Playwright downloads need `programs.nix-ld.enable = true;` and, in
`programs.nix-ld.libraries`, the libraries they load, which nix-ld's default set does not
include (NSS, ALSA, Mesa and GTK among them).

`scripts/test-osauth-linux.sh` tests Linux sign-in against a real polkit and PAM, and the OS
session watch against real D-Bus daemons. Each case runs in a fresh Debian 13 container with
its own D-Bus, polkitd and PAM, and touches nothing of your machine's polkit, D-Bus or PAM. The
script builds the tests in `desktop/target` and leaves the image
`localhost/apprafter-osauth-linux:dev` in podman's store. From the repository root:

```sh
PODMAN='sudo podman' bash scripts/test-osauth-linux.sh               # every case, as CI runs it
PODMAN='sudo podman' bash scripts/test-osauth-linux.sh <test_name>   # one case
```

It needs cargo, cue, the PAM development files, podman and network access. Rootless podman
works only under a systemd user session that delegates the cgroup tree.

`scripts/test-desktop-packages.sh` installs the Linux packages into fresh Debian 13 and Fedora
44 containers and checks them as CI does. Build them first in `desktop/`
(`bun run tauri build --bundles deb,rpm`, or only the format you need), then run it from the
repository root, naming only the packages you built:

```sh
PODMAN='sudo podman' bash scripts/test-desktop-packages.sh \
  desktop/target/release/bundle/deb/*.deb desktop/target/release/bundle/rpm/*.rpm
```

It needs podman and network access. Both scripts fail, rather than skip, when podman is
missing.
