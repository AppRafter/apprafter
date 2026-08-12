// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure filter predicate parser for the interactive machine picker.
//!
//! Parses a raw query string into a list of [`Predicate`]s (AND semantics) and
//! evaluates them against a [`MachineOffer`]. No I/O — only std + `MachineOffer`.

use crate::machine::MachineOffer;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumOp {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Cpu(NumOp, u32),
    Ram(NumOp, f64),
    Disk(NumOp, u32),
    Arch(String),
    CpuType(String),
    Loc(String),
    Sku(String),
    Fuzzy(String),
}

/// Parse a two-char or one-char numeric operator prefix from `s`, returning
/// `(op, rest_of_string)`. Returns `None` if no valid operator is found.
fn parse_num_op(s: &str) -> Option<(NumOp, &str)> {
    if let Some(rest) = s.strip_prefix(">=") {
        Some((NumOp::Ge, rest))
    } else if let Some(rest) = s.strip_prefix("<=") {
        Some((NumOp::Le, rest))
    } else if let Some(rest) = s.strip_prefix('>') {
        Some((NumOp::Gt, rest))
    } else if let Some(rest) = s.strip_prefix('<') {
        Some((NumOp::Lt, rest))
    } else if let Some(rest) = s.strip_prefix('=') {
        Some((NumOp::Eq, rest))
    } else {
        None
    }
}

/// Parse one filter token into a predicate.
/// Unrecognized tokens (or malformed numeric like `cpu>=x`) degrade to `Fuzzy`.
///
/// Ordering matters:
/// 1. Check `cpu`/`cores` + numeric operator (e.g. `cpu>=8`, `cores=4`) BEFORE `cpu:` (keyword).
/// 2. `cpu:dedicated` → `CpuType`; `cpu>=8` → `Cpu` numeric.
fn parse_token(tok: &str) -> Predicate {
    let low = tok.to_lowercase();

    // --- Numeric: cpu / cores ---
    for prefix in &["cpu", "cores"] {
        if let Some(after_prefix) = low.strip_prefix(prefix) {
            if let Some((op, val_str)) = parse_num_op(after_prefix) {
                if let Ok(n) = val_str.parse::<u32>() {
                    return Predicate::Cpu(op, n);
                }
                // malformed number → fall through to Fuzzy below
                return Predicate::Fuzzy(low);
            }
        }
    }

    // --- Numeric: ram ---
    if let Some(after) = low.strip_prefix("ram") {
        if let Some((op, val_str)) = parse_num_op(after) {
            if let Ok(v) = val_str.parse::<f64>() {
                return Predicate::Ram(op, v);
            }
            return Predicate::Fuzzy(low);
        }
    }

    // --- Numeric: disk ---
    if let Some(after) = low.strip_prefix("disk") {
        if let Some((op, val_str)) = parse_num_op(after) {
            if let Ok(v) = val_str.parse::<u32>() {
                return Predicate::Disk(op, v);
            }
            return Predicate::Fuzzy(low);
        }
    }

    // --- Keyword: arch:<value> ---
    if let Some(val) = low.strip_prefix("arch:") {
        return Predicate::Arch(val.to_string());
    }

    // --- Keyword: cpu:<value>  →  CpuType (after numeric cpu*/cores* already handled) ---
    if let Some(val) = low.strip_prefix("cpu:") {
        return Predicate::CpuType(val.to_string());
    }

    // --- Keyword: loc:<value> ---
    if let Some(val) = low.strip_prefix("loc:") {
        return Predicate::Loc(val.to_string());
    }

    // --- Keyword: sku:<value> ---
    if let Some(val) = low.strip_prefix("sku:") {
        return Predicate::Sku(val.to_string());
    }

    // --- Fallback: bare word → fuzzy substring over composite ---
    Predicate::Fuzzy(low)
}

/// Split on whitespace, AND semantics. Empty query → no predicates (matches all).
pub fn parse_query(q: &str) -> Vec<Predicate> {
    q.split_whitespace().map(parse_token).collect()
}

/// Composite string searched by `Fuzzy` predicates.
/// Exactly: location + sku + arch + cpu_type (lowercased).
/// NEVER includes price or latency (M12 constraint).
fn composite(o: &MachineOffer) -> String {
    format!("{} {} {} {}", o.location, o.sku, o.arch, o.cpu_type).to_lowercase()
}

fn apply_num_op_u32(op: NumOp, lhs: u32, rhs: u32) -> bool {
    match op {
        NumOp::Ge => lhs >= rhs,
        NumOp::Gt => lhs > rhs,
        NumOp::Le => lhs <= rhs,
        NumOp::Lt => lhs < rhs,
        NumOp::Eq => lhs == rhs,
    }
}

fn apply_num_op_f64(op: NumOp, lhs: f64, rhs: f64) -> bool {
    match op {
        NumOp::Ge => lhs >= rhs,
        NumOp::Gt => lhs > rhs,
        NumOp::Le => lhs <= rhs,
        NumOp::Lt => lhs < rhs,
        NumOp::Eq => (lhs - rhs).abs() < f64::EPSILON,
    }
}

impl Predicate {
    pub fn matches(&self, o: &MachineOffer) -> bool {
        match self {
            Predicate::Cpu(op, n) => apply_num_op_u32(*op, o.cores, *n),
            Predicate::Ram(op, v) => apply_num_op_f64(*op, o.memory_gb, *v),
            Predicate::Disk(op, n) => apply_num_op_u32(*op, o.disk_gb, *n),
            Predicate::Arch(v) => o.arch.to_lowercase() == *v,
            Predicate::CpuType(v) => o.cpu_type.to_lowercase() == *v,
            Predicate::Loc(v) => o.location.to_lowercase().contains(v.as_str()),
            Predicate::Sku(v) => o.sku.to_lowercase().contains(v.as_str()),
            Predicate::Fuzzy(v) => composite(o).contains(v.as_str()),
        }
    }
}

/// Evaluate all predicates (AND). Empty query matches everything.
pub fn matches_query(q: &str, o: &MachineOffer) -> bool {
    parse_query(q).iter().all(|p| p.matches(o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::MachineOffer;

    fn offer(
        loc: &str,
        sku: &str,
        arch: &str,
        cpu: &str,
        cores: u32,
        ram: f64,
        disk: u32,
    ) -> MachineOffer {
        MachineOffer {
            location: loc.into(),
            sku: sku.into(),
            cores,
            memory_gb: ram,
            disk_gb: disk,
            arch: arch.into(),
            cpu_type: cpu.into(),
            price_monthly_net: Some("249.90".into()),
            price_hourly_net: None,
            available: true,
            recommended: false,
            deprecation: None,
        }
    }

    #[test]
    fn parses_numeric_keyword_and_fuzzy() {
        let o = offer("hel1", "ccx33", "arm", "dedicated", 8, 32.0, 240);
        assert!(matches_query("cpu>=8 ram>=32 arch:arm", &o));
        assert!(matches_query("cores>=8", &o));
        assert!(matches_query("cpu:dedicated", &o));
        assert!(matches_query("loc:hel", &o));
        assert!(matches_query("hel", &o)); // bare token → fuzzy over composite
        assert!(matches_query("sku:ccx", &o));
        assert!(matches_query("disk>=240", &o));
        assert!(!matches_query("ram>=64", &o));
        assert!(!matches_query("arch:x86", &o));
    }

    #[test]
    fn fuzzy_is_composite_only_not_price_or_latency() {
        // composite = location + sku + arch + cpu_type ONLY. price/latency are NOT searchable.
        let o = offer("nbg1", "cx22", "x86", "shared", 2, 4.0, 40);
        assert!(matches_query("nbg1 x86 shared", &o));
        assert!(!matches_query("249", &o)); // price digits must NOT match
    }

    #[test]
    fn numeric_operators() {
        let o = offer("fsn1", "cpx31", "x86", "shared", 4, 8.0, 160);
        assert!(matches_query("cpu=4", &o));
        assert!(matches_query("cpu>3", &o));
        assert!(matches_query("cpu<=4", &o));
        assert!(matches_query("ram<16", &o));
        assert!(!matches_query("cpu>4", &o));
    }

    #[test]
    fn empty_query_matches_everything() {
        let o = offer("nbg1", "cx22", "x86", "shared", 2, 4.0, 40);
        assert!(matches_query("", &o));
        assert!(matches_query("   ", &o));
    }
}
