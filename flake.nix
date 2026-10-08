# SPDX-License-Identifier: FSL-1.1-Apache-2.0
{
  description = "AppRafter platform development environment";

  inputs = {
    # A RELEASE branch, not nixos-unstable. Unstable already carries
    # kubernetes-helm 4.x and nixpkgs has no `kubernetes-helm_3` fallback, so on
    # unstable a routine `nix flake update` would swap the dev shell's helm
    # major — a toolchain change arriving as a side effect of a lockfile
    # refresh, with nothing naming it. The release branch still moves (it is
    # a branch, and flake.lock pins the rev), it just does not cross majors
    # underneath us.
    #
    # HELM 3 HERE, HELM 4 IN CI — ON PURPOSE. Every workflow that installs
    # helm pins 4.3.0 (scripts/upstream-pins.json `tool-helm`), while this
    # shell and the dev container that mirrors it (`devcontainer-helm`) run
    # this branch's helm 3, 3.20. `apprafter` runs whatever helm the user has
    # installed, users have either major, and the two apply a release
    # differently: helm 4 installs server-side, which is why re-running
    # cluster-bootstrap after a loader change conflicts with Argo CD there
    # (WI-369). The CLI has to work under both, and with CI on 4 this
    # tool-belt is where helm 3 still gets run. Two facts bound the split:
    # 3.20 receives no patches (the last was 3.20.2, 2026-04-09; helm 3 went on
    # to 3.22), and helm 3 as a whole gets security fixes only until
    # 2026-11-11. Move the shell and the dev container to helm 4 together
    # before then: the next release branch (nixos-26.11) will carry helm 4,
    # and if it is not out in time, pin a helm release binary here the way
    # `cuePinned` pins cue.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # restic comes from nixpkgs (0.18.1 on nixos-26.05), unlike cue: the
        # CLI's local backup verbs run it against the same repositories the
        # in-cluster runner writes, so what matters is that both are on one
        # MINOR, and the runner's Dockerfile already asserts that minor at
        # build time (`ARG RESTIC_MINOR=`). It is read from there, not written
        # a second time, and the dev shell warns when a lock update moves
        # nixpkgs' restic to another minor.
        resticMinor =
          let
            prefix = "ARG RESTIC_MINOR=";
            lines = pkgs.lib.splitString "\n" (builtins.readFile ./cli/apprafter-backup/Dockerfile);
            arg = pkgs.lib.findFirst (pkgs.lib.hasPrefix prefix) null lines;
          in
          if arg == null then null else pkgs.lib.removePrefix prefix arg;
        resticWarning =
          if resticMinor == null then
            "flake.nix: no `ARG RESTIC_MINOR=` in cli/apprafter-backup/Dockerfile, so the dev shell's restic ${pkgs.restic.version} is checked against nothing."
          else if pkgs.lib.versions.majorMinor pkgs.restic.version != resticMinor then
            "flake.nix: nixpkgs ships restic ${pkgs.restic.version}, but the backup runner asserts ${resticMinor}.x (cli/apprafter-backup/Dockerfile RESTIC_MINOR): the CLI and the runner would work one repository with different restic minors."
          else
            null;

        # CUE is pinned to the SAME version every other place in this repo pins
        # it — the setup-cue inputs in .github/workflows, the CMP sidecar's
        # Dockerfile ARG, and .devcontainer/post-create.sh.
        #
        # It is taken from the upstream release rather than from nixpkgs on
        # purpose. `scripts/crd-check.sh` says it runs under `nix develop` so
        # that cue is "the flake.lock-pinned version: ONE cue version across
        # local and CI" — and until 2026-09 that claim was simply false. The
        # flake gave whatever nixpkgs happened to carry (0.16.1) while CI's
        # setup-cue installed 0.10.0, six minors apart, and the byte-identity
        # gate that sentence exists to justify was comparing output from two
        # different evaluators. Fetching the release binary is what makes the
        # claim true, and it costs one hash instead of a Go rebuild.
        #
        # Bumping cue means editing THIS version and hash together with the
        # workflow inputs, the Dockerfile ARG and the devcontainer script. The
        # hash is the sha256 of the release tarball:
        #   nix-prefetch-url --type sha256 \
        #     https://github.com/cue-lang/cue/releases/download/vX.Y.Z/cue_vX.Y.Z_linux_amd64.tar.gz
        # Written WITH the leading "v" so it is byte-identical to the string
        # every other cue pin in this repo carries — the version watcher
        # compares the captured strings literally and reports a mismatch as
        # drift, which is the right behaviour and which a bare "0.17.1" here
        # would trip on every run.
        cueVersion = "v0.17.1";
        cuePinned = pkgs.stdenv.mkDerivation {
          pname = "cue";
          version = pkgs.lib.removePrefix "v" cueVersion;
          src = pkgs.fetchurl {
            url = "https://github.com/cue-lang/cue/releases/download/${cueVersion}/cue_${cueVersion}_${
              if pkgs.stdenv.hostPlatform.isDarwin then "darwin" else "linux"
            }_${if pkgs.stdenv.hostPlatform.isAarch64 then "arm64" else "amd64"}.tar.gz";
            sha256 =
              {
                "x86_64-linux" = "sha256-o5sMl2lQadldJ22Zvg9dursIHYAb/cm6Sbdu+vlOI2k=";
              }
              .${system} or (throw "flake.nix: no cue ${cueVersion} hash recorded for ${system} — add one beside the x86_64-linux entry");
          };
          sourceRoot = ".";
          # A single static binary; there is nothing to build or patch.
          dontBuild = true;
          dontConfigure = true;
          installPhase = "install -Dm755 cue $out/bin/cue";
        };

        # AppRafter Desktop (ADR 0067). A SEPARATE shell so the default one does not
        # pull a WebKitGTK/GTK closure. x86_64-linux only — macOS/Windows desktop work
        # uses rustup + bun (design spec §6.1). The Tauri CLI comes from desktop/bun.lock
        # (`bun run tauri`), NOT nixpkgs' cargo-tauri (one minor behind): tauri-cli
        # refuses a build whose crate and @tauri-apps/api minors disagree. Rust comes
        # from rustup (desktop/rust-toolchain.toml), which nixpkgs' rustc would ignore.
        # bun comes from mise (mise.toml pins 1.4) or the user's own install, the same
        # way: nixpkgs' bun is 1.3 at the locked rev and cannot read desktop/bun.lock
        # (lockfileVersion 2, written by bun 1.4). The shellHook warns, never fails,
        # when the bun on PATH is not 1.4.x.
        desktopShell = pkgs.mkShell {
          name = "apprafter-desktop";
          nativeBuildInputs = with pkgs; [ pkg-config wrapGAppsHook3 ];
          buildInputs = with pkgs; [
            webkitgtk_4_1 gtk3 libsoup_3 librsvg glib-networking
            libayatana-appindicator gsettings-desktop-schemas dbus
          ];
          packages = with pkgs; [ cuePinned just jq git xvfb-run ];
          shellHook = ''
            # GTK file chooser and HiDPI scale need the schemas (NixOS wiki, Tauri).
            export XDG_DATA_DIRS="$GSETTINGS_SCHEMAS_PATH''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
            export GIO_MODULE_DIR="${pkgs.glib-networking}/lib/gio/modules/"
            # Every library the app loads, PREPENDED. Rust >= 1.90 links x86_64-linux-gnu
            # with its bundled rust-lld, which bypasses nixpkgs' ld-wrapper, so a binary
            # built in this shell gets no RUNPATH into /nix/store and dies with
            # `libgobject-2.0.so.0: cannot open shared object file`. Prepended, not
            # appended: an ambient LD_LIBRARY_PATH (a NixOS user profile) can carry a
            # DIFFERENT webkitgtk build, and that one must not win. libappindicator-sys
            # also dlopen()s libayatana-appindicator3 at run time. (atk ships inside
            # at-spi2-core.)
            export LD_LIBRARY_PATH="${
              pkgs.lib.makeLibraryPath (
                with pkgs;
                [
                  webkitgtk_4_1 gtk3 libsoup_3 glib cairo pango gdk-pixbuf harfbuzz
                  at-spi2-core librsvg dbus libayatana-appindicator
                ]
              )
            }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            case "$(bun --version 2>/dev/null)" in
              1.4.*) ;;
              *) echo "apprafter-desktop: desktop/bun.lock needs bun 1.4.x, found '$(bun --version 2>/dev/null || echo none)' — install it with mise (mise.toml) or from bun.sh" >&2 ;;
            esac
          '';
        };
      in
      {
        devShells = {
          default = pkgs.lib.warnIf (resticWarning != null) resticWarning (pkgs.mkShell {
            name = "apprafter";

            packages = with pkgs; [
              # Configuration language — the version pinned above, NOT nixpkgs'.
              # See the `cuePinned` comment for why.
              cuePinned

              # Argo CD ships custom resource health as Lua in `argocd-cm`, and a
              # broken script fails SILENTLY — Argo logs it and falls back. This
              # is what `scripts/check-argocd-health-lua.sh` runs them under.
              lua

              # Rust toolchain
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer

              # Dependency health, which the version watcher cannot see: an
              # abandoned crate sits on its own final release forever and so is
              # never "behind". `scripts/cargo-deny.sh` falls back to
              # `nix run nixpkgs#cargo-deny` without this, but having it in the
              # shell keeps the dev-loop run fast.
              cargo-deny

              # JavaScript / TypeScript runtime (for Backstage tooling)
              bun

              # Kubernetes tooling
              kubectl
              k9s
              kubernetes-helm
              k3d
              kind # local e2e cluster on podman (rootless) — k3d needs docker
              argocd
              cilium-cli

              # Talos / bare-metal
              talosctl

              # Container build / supply-chain
              cosign
              syft
              trivy
              grype

              # Repo tooling
              just
              lefthook
              age
              sops
              jq
              git

              # Backups: `apprafter backup create/list/show/check/prune`,
              # `restore` and `export` run restic locally. Held to the runner's
              # minor by `resticWarning` above.
              restic

              # Documentation site. ONE python env — `nix shell
              # nixpkgs#python3Packages.mkdocs-material` ships no `mkdocs`
              # binary, and adding `python3Packages.mkdocs` alongside it
              # yields a second env whose site-packages lacks the theme
              # ("Unrecognised theme name: 'material'"). literate-nav
              # renders the generated CLI reference nav (docsgen SUMMARY.md);
              # redirects is installed for the W5 IA move.
              (python3.withPackages (ps: [
                ps.mkdocs-material
                ps.mkdocs-literate-nav
                ps.mkdocs-redirects
              ]))
            ];

            # mkdocs-material 9.7.6 prints a red MkDocs-2.0 advocacy banner on
            # EVERY mkdocs invocation (material/templates/__init__.py gates it
            # on this variable). It is upstream's opinion of a framework
            # release this site does not run — the line readers react to,
            # "Currently unlicensed - unsuitable for production use", is about
            # MkDocs 2.0 and not about the theme here.
            #
            # Silenced in the devShell rather than in the Justfile because
            # every call site goes through `nix develop`: `just docs-serve`,
            # `just docs-build`, `scripts/docs-check.sh` and release-docs.yml.
            # Three prefixed recipes would leave the CI logs noisy and would
            # miss any future `nix develop --command mkdocs ...` typed by hand.
            #
            # It also shrinks a real hazard: scripts/docs-check.sh tees mkdocs
            # stderr into a file it then pattern-matches, so third-party output
            # in that stream is one grep collision away from a false failure.
            NO_MKDOCS_2_WARNING = "1";

            shellHook = ''
              echo "AppRafter dev shell ready."
              echo
              echo "Useful commands:"
              echo "  just --list      # available targets"
              echo "  just bootstrap   # install git hooks"
              echo "  just lint        # CUE + SPDX + docs + conditional Rust/TS"
              echo "  just e2e-up      # local k3d cluster"
              echo
            '';
          });
        }
        // pkgs.lib.optionalAttrs (system == "x86_64-linux") { desktop = desktopShell; };

        # Exposed so `scripts/cue` can reach the pinned binary WITHOUT
        # `nix develop`, whose shellHook prints a banner onto stdout and would
        # corrupt every caller that captures cue's output.
        packages.cue = cuePinned;

        formatter = pkgs.nixfmt;
      }
    );
}
