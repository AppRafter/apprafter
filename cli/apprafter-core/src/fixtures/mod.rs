// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What a cluster answers, for tests (D.5 overview B76): the objects `apprafter status`, `top`,
//! `platform status`, `platform autoscale show`, `platform env show` and `node status` read, in
//! the shapes the apiserver, metrics-server, the kubelet and the node's shell give them.
//!
//! Every builder that writes a time takes `now` and puts each time at an offset in the middle
//! of the band the CLI's relative words give it ([`time::Ago`]). The CLI goldens pass the real
//! clock (B55), the desktop's mock fixtures a fixed epoch (d5-g): a golden reads the same for
//! as long as [`time::Ago::margin`] after its fixture was built, and a committed mock never
//! moves. No builder reads a clock, a file or the environment.
//!
//! Test-only: `test-support`, which only `[dev-dependencies]` enable, and the core's own tests.

pub mod apps;
pub mod node;
pub mod plans;
pub mod platform;
pub mod time;
pub mod top;

use serde_json::{json, Value};

/// A `kubectl get … -o json` answer holding `items`, as the apiserver wraps a list.
pub fn list(items: Vec<Value>) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "List",
        "metadata": { "resourceVersion": "" },
        "items": items
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_wraps_its_items() {
        let wrapped = list(vec![json!({ "a": 1 }), json!({ "b": 2 })]);
        assert_eq!(wrapped["kind"], "List");
        assert_eq!(wrapped["items"], json!([{ "a": 1 }, { "b": 2 }]));
        assert_eq!(list(vec![])["items"], json!([]));
    }
}
