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

            # Rust toolchain
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer

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

            # Documentation site (TechDocs / mkdocs-material).
            python3Packages.mkdocs-material
          ];

          shellHook = ''
            echo "AppRafter dev shell ready."
            echo
            echo "Useful commands:"
            echo "  just --list      # available targets"
            echo "  just bootstrap   # install git hooks"
            echo "  just lint        # CUE + SPDX + conditional Rust/TS"
            echo "  just e2e-up      # local k3d cluster"
            echo
          '';
        };

        formatter = pkgs.nixfmt;
      }
    );
}
