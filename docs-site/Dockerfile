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

EXPOSE 80

# Healthcheck: hit / and expect HTTP 200.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget -q -O /dev/null http://localhost/ || exit 1
