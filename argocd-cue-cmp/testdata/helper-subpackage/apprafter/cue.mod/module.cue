// SPDX-License-Identifier: MIT
//
// CUE module for the helper-sub-package fixture. Committed (not
// injected) because the manifest imports its sibling `lib` package by
// MODULE PATH, which needs a stable module name. The entrypoint reuses
// an existing cue.mod and injects only `pkg/` into it.

module: "helper-subpackage.cue-cmp.apprafter.io"

language: {
	version: "v0.10.0"
}
