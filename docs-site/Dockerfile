# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# docs image — the MkDocs-Material site served by Caddy.
#
# Build context: THIS directory, with `site/` already built into it:
#
#   nix develop --command mkdocs build --strict --site-dir docs-site/site
#   docker build -t docs -f docs-site/Dockerfile docs-site/
#
# `site/` is a build artefact and is gitignored (.gitignore's `site/`
# rule is unanchored, so it covers this path as well as the repo-root
# one mkdocs writes by default).
#
# ---------------------------------------------------------------------
# There is deliberately NO build stage here. Do not add one.
# ---------------------------------------------------------------------
# The obvious "improvement" to this file is a first stage that runs
# `pip install mkdocs-material && mkdocs build`, removing the
# prerequisite above. That would introduce a SECOND, UNPINNED toolchain
# which produces the *published* site, while every gate in this repo
# validates a site built by a different one.
#
# Subphase 2.19a exists because a byte-compared artefact needs one
# pinned toolchain across a contributor's machine and CI: `flake.nix`
# defines a single `python3.withPackages` environment, and
# `scripts/docs-check.sh` builds under it with `--strict`. A Dockerfile
# that built the site independently would be that invariant's first
# breach, and the drift would be silent — both builds succeed, and only
# the published bytes differ.
#
# So the build happens once, under `nix develop`, in
# `.github/workflows/release-docs.yml`; this file only copies the
# result. If you want the image to build itself, change the flake, not
# this file.

FROM caddy:2-alpine

COPY site /usr/share/caddy
COPY Caddyfile /etc/caddy/Caddyfile

# Fail the BUILD on a Caddyfile the server cannot load.
#
# Nothing else in this repository reads this file: `just lint` does not,
# `scripts/docs-check.sh` never looks below docs/, and there is no
# `caddy` on a contributor's PATH to run by hand. Without this layer a
# syntax error, an unknown directive or a bad matcher ships a green
# build and a container that exits on start — the image is only
# exercised when it is already published and Argo CD has rolled it.
#
# The check costs nothing to add because the binary is already here:
# `caddy` is the entrypoint of this base image. It parses and
# provisions the config without binding a port, so it is safe in a
# build sandbox. It is a SYNTAX and LOADABILITY check only — it says
# nothing about whether the rules serve what they should, which is what
# the smoke step in .github/workflows/release-docs.yml is for.
#
# Prove it fires before trusting it:
#
#   printf 'not a directive\n' >> docs-site/Caddyfile
#   docker build -f docs-site/Dockerfile docs-site/   # must FAIL here
RUN caddy validate --adapter caddyfile --config /etc/caddy/Caddyfile

EXPOSE 80

# Healthcheck: hit / and expect HTTP 200.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -q -O /dev/null http://localhost/ || exit 1
