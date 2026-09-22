# SPDX-License-Identifier: FSL-1.1-Apache-2.0
{
  description = "AppRafter platform development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
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
      in
      {
        devShells.default = pkgs.mkShell {
          name = "apprafter";

          packages = with pkgs; [
            # Configuration language
            cue

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

        formatter = pkgs.nixfmt;
      }
    );
}
