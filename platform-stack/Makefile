# SPDX-License-Identifier: FSL-1.1-Apache-2.0
#
# platform-stack — local development helper.
#
# `cue cmd render` is the actual renderer; this Makefile wraps
# it so contributors can type `make render` from `platform-stack/`
# without remembering the CUE invocation, and so the CI publish
# workflow (`.github/workflows/platform-stack-publish.yml`,
# lands in plan.md sub-phase 1.68) has a single command to call.

# scripts/cue is the single resolver for this repo's PINNED cue. The old
# `command -v cue || nix run nixpkgs#cue --` picked whatever the machine or
# nixpkgs happened to carry, which is not what flake.nix pins.
CUE := $(CURDIR)/../scripts/cue

# Resolve helm similarly. Optional — `make render-only` skips
# the lint step.
HELM := $(shell command -v helm 2>/dev/null || echo "nix run nixpkgs#kubernetes-helm --")

# Chart version — read from the CUE source so we never hand-edit
# a path here.
VERSION := $(shell $(CUE) export ./cue/... -e tier1.version --out text 2>/dev/null)
DIST := dist/platform-stack-$(VERSION)

.PHONY: render render-only lint clean help

help:                       ## Show this help.
	@awk 'BEGIN {FS = ":.*##"} /^[a-zA-Z_-]+:.*##/ { printf "  %-12s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

render: render-only lint    ## Render CUE → dist/ + helm lint.

render-only:                ## Render CUE → dist/ (no lint).
	@rm -rf dist
	@$(CUE) cmd render ./cue/...
	@echo "rendered: $(DIST)"

lint:                       ## Run helm lint on the rendered chart.
	@$(HELM) lint $(DIST)

clean:                      ## Remove the rendered output.
	@rm -rf dist
