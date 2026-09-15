// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Kubernetes resource quantities, in and out.
//!
//! Kubernetes writes CPU and memory in a format built for machines: `1940m`,
//! `3865468Ki`, and — when something serialises a computed value rather than
//! a declared one — a bare `183046954`. The last of those is the shape a VPA
//! recommendation arrives in, and a column reading
//! `limits.memory: 183046954` asks a reader to divide by 1024 twice before
//! they know whether it is large.
//!
//! So: parse every shape the apiserver emits, and print one shape back.
//!
//! **Not shared with the operator's `quantity_bytes`.** `cli/` and
//! `operator/` are separate Cargo workspaces with no common dependency, and
//! the operator's own copy carries the same note for the same reason. The
//! suffix table below is deliberately identical to it — a platform that
//! disagrees with itself about what `Gi` means would be worse than either
//! answer.

/// Parse a quantity to bytes: binary SI (`Ki/Mi/Gi/Ti/Pi`), decimal SI
/// (`k/M/G/T/P`), or a bare number. `None` on anything else.
///
/// Fractional inputs are real — `0.5Gi` is legal — so the arithmetic is
/// floating point and the result is rounded rather than truncated.
pub fn parse_bytes(q: &str) -> Option<i64> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    let idx = q.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(q.len());
    let (num, unit) = q.split_at(idx);
    let n: f64 = num.parse().ok()?;
    let mul: f64 = match unit {
        "" => 1.0,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        _ => return None,
    };
    Some((n * mul).round() as i64)
}

/// Parse a CPU quantity to millicores: `2` → `2000`, `500m` → `500`,
/// `0.5` → `500`, `412500000n` → `413`. `None` on anything else.
///
/// `m` means milli here and nowhere else in this module — for memory the
/// same letter is a decimal-SI milli-byte, which nothing emits. Keeping the
/// two parsers separate is what stops `500m` of CPU being read as half a
/// byte.
///
/// **`n` and `u` are not decoration.** `metrics.k8s.io` reports CPU in
/// NANOCORES (`"412500000n"`) — every node and every pod, on every
/// cluster that runs metrics-server; `kubectl top` divides them down
/// before printing. A parser that knows only `m` answers `None` on the
/// entire metrics API, and a `None` folded to zero shows a saturated node
/// as idle. `u` (micro) is in the same suffix table and costs one arm.
pub fn parse_millicores(q: &str) -> Option<i64> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    // Longest suffix first: `m` is a prefix of nothing here, but the
    // order is what keeps it that way if a suffix is ever added.
    for (suffix, milli_per_unit) in [("n", 1e-6), ("u", 1e-3), ("m", 1.0)] {
        if let Some(stripped) = q.strip_suffix(suffix) {
            let n: f64 = stripped.parse().ok()?;
            return Some((n * milli_per_unit).round() as i64);
        }
    }
    let n: f64 = q.parse().ok()?;
    Some((n * 1000.0).round() as i64)
}

/// Render a byte count the way a reader places a size: `175Mi`, `3.7Gi`.
///
/// Binary units, because that is what a node reports and what a limit is
/// written in — showing `184MB` next to a `256Mi` limit invites a
/// comparison that is off by five percent.
///
/// One decimal below 10 in a unit and none above, so a column stays narrow
/// while `3.7Gi` and `3.8Gi` remain distinguishable. Bytes and Ki never get
/// a decimal: a fractional byte is noise.
pub fn humanise_bytes(bytes: i64) -> String {
    const UNITS: [(&str, f64); 5] = [
        ("Pi", 1.125_899_906_842_624e15),
        ("Ti", 1.099_511_627_776e12),
        ("Gi", 1.073_741_824e9),
        ("Mi", 1.048_576e6),
        ("Ki", 1024.0),
    ];
    let neg = bytes < 0;
    let abs = bytes.unsigned_abs() as f64;
    let sign = if neg { "-" } else { "" };
    for (unit, scale) in UNITS {
        if abs >= scale {
            let v = abs / scale;
            // `Ki` is already fine-grained; a decimal there is noise.
            return if v < 10.0 && unit != "Ki" {
                format!("{sign}{v:.1}{unit}")
            } else {
                format!("{sign}{:.0}{unit}", v)
            };
        }
    }
    format!("{sign}{:.0}", abs)
}

/// Render millicores: `1940m` below a core, `2.4` at or above one.
///
/// The crossover exists because a reader thinks in cores once there is more
/// than one, and in millicores below that — `0.085` and `85m` describe the
/// same slice, and only one of them reads like a small number.
pub fn humanise_millicores(milli: i64) -> String {
    if milli.abs() < 1000 {
        return format!("{milli}m");
    }
    let cores = milli as f64 / 1000.0;
    if cores.fract().abs() < 0.05 {
        format!("{cores:.0}")
    } else {
        format!("{cores:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_every_shape_the_apiserver_emits_for_memory() {
        assert_eq!(parse_bytes("1024"), Some(1024));
        assert_eq!(parse_bytes("1Ki"), Some(1024));
        assert_eq!(parse_bytes("1Mi"), Some(1_048_576));
        assert_eq!(parse_bytes("1Gi"), Some(1_073_741_824));
        assert_eq!(parse_bytes("1k"), Some(1_000));
        assert_eq!(parse_bytes("1M"), Some(1_000_000));
        // A node reports its memory in Ki, and it is not a round number.
        assert_eq!(parse_bytes("3865468Ki"), Some(3_958_239_232));
        // Fractions are legal and must not truncate to zero.
        assert_eq!(parse_bytes("0.5Gi"), Some(536_870_912));
        assert_eq!(parse_bytes(" 256Mi "), Some(268_435_456));
    }

    #[test]
    fn a_quantity_it_cannot_read_is_none_rather_than_a_guess() {
        for bad in ["", "   ", "Gi", "12Xi", "abc", "1.2.3"] {
            assert_eq!(parse_bytes(bad), None, "{bad} parsed");
        }
    }

    #[test]
    fn cpu_is_read_in_millicores_however_it_is_written() {
        assert_eq!(parse_millicores("2"), Some(2000));
        assert_eq!(parse_millicores("500m"), Some(500));
        assert_eq!(parse_millicores("0.5"), Some(500));
        assert_eq!(parse_millicores("1940m"), Some(1940));
        assert_eq!(parse_millicores("0"), Some(0));
        assert_eq!(parse_millicores("25m"), Some(25));
    }

    #[test]
    fn the_metrics_api_reports_cpu_in_nanocores_and_it_must_not_read_as_nothing() {
        // Every `metrics.k8s.io` node and pod sample arrives like this.
        // Answering `None` here — and folding that to zero — is how a
        // saturated node renders as idle.
        assert_eq!(parse_millicores("412500000n"), Some(413));
        assert_eq!(parse_millicores("31000000n"), Some(31));
        assert_eq!(parse_millicores("0"), Some(0));
        // Below half a millicore rounds to zero, which is the truthful
        // reading of a pod that is genuinely doing nothing.
        assert_eq!(parse_millicores("400000n"), Some(0));
        assert_eq!(parse_millicores("2500u"), Some(3));
    }

    #[test]
    fn the_same_letter_means_different_things_for_cpu_and_memory() {
        // `500m` is half a CPU and is NOT half a byte. Reading one parser's
        // input with the other is the bug this pair of asserts exists to
        // make loud.
        assert_eq!(parse_millicores("500m"), Some(500));
        assert_eq!(parse_bytes("500m"), None);
    }

    #[test]
    fn the_vpa_recommendation_from_the_bug_report_becomes_readable() {
        // The reported line was `limits.memory: 183046954`.
        assert_eq!(humanise_bytes(parse_bytes("183046954").unwrap()), "175Mi");
    }

    #[test]
    fn sizes_render_with_one_decimal_only_where_it_distinguishes() {
        assert_eq!(humanise_bytes(0), "0");
        assert_eq!(humanise_bytes(512), "512");
        assert_eq!(humanise_bytes(1024), "1Ki");
        assert_eq!(humanise_bytes(4096), "4Ki");
        assert_eq!(humanise_bytes(1_048_576), "1.0Mi");
        assert_eq!(humanise_bytes(268_435_456), "256Mi");
        assert_eq!(humanise_bytes(3_958_239_232), "3.7Gi");
        assert_eq!(humanise_bytes(1_099_511_627_776), "1.0Ti");
    }

    #[test]
    fn a_negative_size_keeps_its_sign_rather_than_wrapping() {
        // `free` is computed by subtraction and goes negative on an
        // over-committed node, which is exactly when it must be readable.
        assert_eq!(humanise_bytes(-1_073_741_824), "-1.0Gi");
        assert_eq!(humanise_bytes(-512), "-512");
    }

    #[test]
    fn cpu_renders_in_millicores_below_a_core_and_cores_above() {
        assert_eq!(humanise_millicores(0), "0m");
        assert_eq!(humanise_millicores(25), "25m");
        assert_eq!(humanise_millicores(999), "999m");
        assert_eq!(humanise_millicores(1000), "1");
        assert_eq!(humanise_millicores(1940), "1.9");
        assert_eq!(humanise_millicores(2000), "2");
        assert_eq!(humanise_millicores(-250), "-250m");
    }

    #[test]
    fn every_parsed_quantity_survives_a_round_trip_through_its_own_printer() {
        // Not exact equality — the printers round on purpose — but the
        // reading must land in the right unit and the right order of
        // magnitude, which is what a table is read for.
        for (raw, want) in [("256Mi", "256Mi"), ("1Gi", "1.0Gi"), ("3865468Ki", "3.7Gi")] {
            assert_eq!(humanise_bytes(parse_bytes(raw).unwrap()), want, "{raw}");
        }
        for (raw, want) in [("2", "2"), ("1940m", "1.9"), ("25m", "25m")] {
            assert_eq!(
                humanise_millicores(parse_millicores(raw).unwrap()),
                want,
                "{raw}"
            );
        }
    }
}
