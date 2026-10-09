// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The cluster API (D.3 overview §3.9): one minimal trait, `Kube`, whose one implementation runs
//! `kubectl` (D.3c); D.5 adds methods.

use serde::Serialize;

use crate::cancel::CancellationToken;
use crate::error::CoreResult;

/// Why a cluster API request failed, as the CLI classifies `kubectl`'s errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum KubeErrorKind {
    Unreachable,
    Forbidden,
    KindNotServed,
    ObjectNotFound,
    Other,
}

/// One to one; a timeout is mapped to `Unreachable` by the caller (D.3c).
impl From<cli_core::diagnose::KubectlFailure> for KubeErrorKind {
    fn from(f: cli_core::diagnose::KubectlFailure) -> Self {
        use cli_core::diagnose::KubectlFailure as F;
        match f {
            F::Unreachable => Self::Unreachable,
            F::Forbidden => Self::Forbidden,
            F::KindNotServed => Self::KindNotServed,
            F::ObjectNotFound => Self::ObjectNotFound,
            F::Other => Self::Other,
        }
    }
}

impl KubeErrorKind {
    /// The snake_case name the desktop's error fields carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Forbidden => "forbidden",
            Self::KindNotServed => "kind_not_served",
            Self::ObjectNotFound => "object_not_found",
            Self::Other => "other",
        }
    }
}

/// The apiserver's version, and how long asking took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct KubeVersion {
    pub git_version: String,
    pub elapsed_ms: u64,
}

/// The cluster API, as the core uses it.
pub trait Kube: Send + Sync {
    /// `GET /version` within the context's request timeout. D.5 adds methods.
    fn server_version(&self, cancel: &CancellationToken) -> CoreResult<KubeVersion>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kube_error_kinds_mirror_the_kubectl_classification() {
        use cli_core::diagnose::KubectlFailure as F;
        assert_eq!(
            [
                F::Unreachable,
                F::Forbidden,
                F::KindNotServed,
                F::ObjectNotFound,
                F::Other
            ]
            .map(KubeErrorKind::from)
            .map(KubeErrorKind::as_str),
            [
                "unreachable",
                "forbidden",
                "kind_not_served",
                "object_not_found",
                "other"
            ]
        );
    }
}
