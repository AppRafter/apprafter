// SPDX-License-Identifier: FSL-1.1-MIT
//! Pure rendering function: `Application` -> Vec of k8s resources.
//!
//! v0.1.26 ships an empty stub. The actual Deployment / Service /
//! HTTPRoute rendering logic lands in v0.1.29 (phase 1.9) along with
//! per-environment unification via CUE.

use operator_core::Application;
use serde_json::Value;

/// Render the resources that compose an Application (Deployment +
/// Service + HTTPRoute, plus any side artefacts). v0.1.26 returns
/// an empty Vec — phase 1.9 fills it in with real logic.
pub fn render_application(_app: &Application) -> Vec<Value> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::ApplicationSpec;

    #[test]
    fn render_returns_empty_vec_for_now() {
        let app = Application::new("test", ApplicationSpec::default());
        assert!(render_application(&app).is_empty());
    }
}
