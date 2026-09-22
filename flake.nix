# SPDX-License-Identifier: FSL-1.1-Apache-2.0
{
  description = "AppRafter platform development environment";

  inputs = {
    # A RELEASE branch, not nixos-unstable. Unstable already carries
    # kubernetes-helm 4.x and nixpkgs has no `kubernetes-helm_3` fallback, so a
    # routine `nix flake update` would have put Helm 4 in the dev shell while
    # CI runs Helm 3 — a toolchain swap arriving as a side effect of a lockfile
    # refresh, with nothing naming it. The release branch still moves (it is
    # a branch, and flake.lock pins the rev), it just does not cross majors
    # underneath us.
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
      in
      {
        devShells.default = pkgs.mkShell {
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
        };

        # Exposed so `scripts/cue` can reach the pinned binary WITHOUT
        # `nix develop`, whose shellHook prints a banner onto stdout and would
        # corrupt every caller that captures cue's output.
        packages.cue = cuePinned;

        formatter = pkgs.nixfmt;
      }
    );
}
