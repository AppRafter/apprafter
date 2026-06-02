// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! kube-rs Controller for v1alpha1 `ResourceClaim` (Phase 2.3) —
//! matches each claim to a ServiceProvider. Full controller wiring
//! lands in later 2.3 tasks; this scaffold ships the pure matcher.

pub mod matching;
