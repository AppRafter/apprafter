// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure machine-picker helpers: row type, ASCII renderer, rank scorer, sort keys.
//!
//! NO I/O — only std + `MachineOffer`/`matches_query`. The interactive `inquire`
//! flow is added in a separate task on top of these pure helpers.

// The public API in this module is intentionally unused until the interactive
// picker (next task) wires it into the CLI command graph.
#![allow(dead_code)]

use std::cmp::Ordering;

use cli_providers::machine::MachineOffer;
use cli_providers::machine_filter::matches_query;

/// A catalog offer plus UI-only measured latency (the picker's row).
#[derive(Debug, Clone)]
pub struct MachineRow {
    pub offer: MachineOffer,
    pub latency_ms: Option<u32>,
}

/// Picker options: a real row, or the "reveal sold-out" toggle sentinel.
///
/// `Row` is boxed to keep the enum size reasonable (MachineRow is ~224 bytes;
/// the sentinel variant is zero-size, so without boxing the enum wastes 224B
/// everywhere a `ShowSoldOut` is stored).
#[derive(Debug, Clone)]
pub enum PickerChoice {
    Row(Box<MachineRow>),
    ShowSoldOut,
}

impl PickerChoice {
    /// Convenience constructor — avoids callers writing `Box::new(...)` directly.
    pub fn row(r: MachineRow) -> Self {
        PickerChoice::Row(Box::new(r))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortKey {
    LatencyAsc,
    PriceAsc,
    CoresDesc,
    RamDesc,
    DiskDesc,
    Location,
}

/// Score a picker choice for the fuzzy-ranked list.
///
/// - `PickerChoice::ShowSoldOut` → always `Some(i64::MIN)` (pinned bottom, filter-immune).
/// - `PickerChoice::Row(r)` → if the offer matches `input`, `Some(rows_len - idx)`
///   (higher = earlier in list, preserves base order); else `None` (hide).
pub fn score_choice(
    input: &str,
    choice: &PickerChoice,
    rows_len: usize,
    idx: usize,
) -> Option<i64> {
    match choice {
        PickerChoice::ShowSoldOut => Some(i64::MIN),
        PickerChoice::Row(r) => {
            if matches_query(input, &r.offer) {
                Some(rows_len as i64 - idx as i64)
            } else {
                None
            }
        }
    }
}

/// Render one machine row as a single ASCII line, ≤ 64 chars.
///
/// Column layout (all ASCII, space-separated):
/// ```text
/// LOC   LAT   SKU      SPEC           ARCH  PRICE   FLAG
/// hel1  12ms  ccx23    8c/32G/240G    x86   49.90   *
/// ```
/// Widths: loc=5 lat=5 sku=8 spec=14 arch=4 price=7 flag=1 + 6 separators = 50 ≤ 64
/// Units (EUR, ms, /mo) go in the legend printed by the interactive task.
/// Marker precedence: `!` (deprecating) > `*` (recommended) > ` ` (none).
pub fn render_row(r: &MachineRow) -> String {
    let loc = format!("{:<5}", &r.offer.location);
    let lat = match r.latency_ms {
        Some(ms) => format!("{:<5}", format!("{}ms", ms)),
        None => format!("{:<5}", "n/a"),
    };
    let sku = format!("{:<8}", &r.offer.sku);
    let spec = format!(
        "{:<14}",
        format!(
            "{}c/{}G/{}G",
            r.offer.cores, r.offer.memory_gb as u32, r.offer.disk_gb
        )
    );
    let arch = format!("{:<4}", &r.offer.arch);
    let price = format!("{:<7}", r.offer.price_monthly_net.as_deref().unwrap_or("-"));
    let flag = if r.offer.deprecation.is_some() {
        "!"
    } else if r.offer.recommended {
        "*"
    } else {
        " "
    };
    format!("{loc} {lat} {sku} {spec} {arch} {price} {flag}")
}

impl std::fmt::Display for MachineRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", render_row(self))
    }
}

impl std::fmt::Display for PickerChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PickerChoice::Row(r) => write!(f, "{}", render_row(r)),
            PickerChoice::ShowSoldOut => write!(f, "-- show sold-out --"),
        }
    }
}

/// Sort a slice of `MachineRow` in-place by the given key.
///
/// - `LatencyAsc`: missing latency sorts last (`u32::MAX`).
/// - `PriceAsc`: unparseable / missing price sorts last (`f64::INFINITY`), rows still kept.
/// - `CoresDesc` / `RamDesc` / `DiskDesc`: descending numeric.
/// - `Location`: ascending alphabetical.
pub fn sort_rows(rows: &mut [MachineRow], key: SortKey) {
    match key {
        SortKey::LatencyAsc => {
            rows.sort_by_key(|r| r.latency_ms.unwrap_or(u32::MAX));
        }
        SortKey::PriceAsc => {
            rows.sort_by(|a, b| {
                let pa = parse_price(&a.offer.price_monthly_net);
                let pb = parse_price(&b.offer.price_monthly_net);
                pa.partial_cmp(&pb).unwrap_or(Ordering::Equal)
            });
        }
        SortKey::CoresDesc => {
            rows.sort_by(|a, b| b.offer.cores.cmp(&a.offer.cores));
        }
        SortKey::RamDesc => {
            rows.sort_by(|a, b| {
                b.offer
                    .memory_gb
                    .partial_cmp(&a.offer.memory_gb)
                    .unwrap_or(Ordering::Equal)
            });
        }
        SortKey::DiskDesc => {
            rows.sort_by(|a, b| b.offer.disk_gb.cmp(&a.offer.disk_gb));
        }
        SortKey::Location => {
            rows.sort_by(|a, b| a.offer.location.cmp(&b.offer.location));
        }
    }
}

fn parse_price(p: &Option<String>) -> f64 {
    p.as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(f64::INFINITY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_providers::machine::MachineOffer;

    /// Build a minimal `MachineOffer`. Split into two helpers to stay within
    /// clippy's 7-argument limit.
    fn make_offer(
        loc: &str,
        sku: &str,
        arch: &str,
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
            cpu_type: "shared".into(),
            price_monthly_net: None,
            price_hourly_net: None,
            available: true,
            recommended: false,
            deprecation: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn offer(
        loc: &str,
        sku: &str,
        arch: &str,
        cores: u32,
        ram: f64,
        disk: u32,
        price: Option<&str>,
        avail: bool,
        rec: bool,
    ) -> MachineOffer {
        let mut o = make_offer(loc, sku, arch, cores, ram, disk);
        o.price_monthly_net = price.map(|s| s.into());
        o.available = avail;
        o.recommended = rec;
        o
    }

    fn row(o: MachineOffer, lat: Option<u32>) -> MachineRow {
        MachineRow {
            offer: o,
            latency_ms: lat,
        }
    }

    #[test]
    fn rank_scorer_keeps_base_order_and_filters() {
        let rows = [
            row(
                offer(
                    "hel1",
                    "ccx23",
                    "x86",
                    8,
                    32.0,
                    240,
                    Some("49.90"),
                    true,
                    false,
                ),
                Some(12),
            ),
            row(
                offer("nbg1", "cx22", "x86", 2, 4.0, 40, Some("3.92"), true, false),
                Some(20),
            ),
        ];
        let len = rows.len(); // 2
                              // kept row at idx 0 → Some(len-0)=Some(2); idx 1 filtered by cpu>=8 → None
        assert_eq!(
            score_choice("cpu>=8", &PickerChoice::row(rows[0].clone()), len, 0),
            Some(2)
        );
        assert_eq!(
            score_choice("cpu>=8", &PickerChoice::row(rows[1].clone()), len, 1),
            None
        );
        // no filter → both kept, idx-descending rank preserves base order
        assert_eq!(
            score_choice("", &PickerChoice::row(rows[1].clone()), len, 1),
            Some(1)
        );
        // sentinel always pinned to the BOTTOM (i64::MIN), immune to the filter text
        assert_eq!(
            score_choice("cpu>=999", &PickerChoice::ShowSoldOut, len, len),
            Some(i64::MIN)
        );
    }

    #[test]
    fn render_row_is_ascii_and_within_budget() {
        let r = row(
            offer(
                "fsn1",
                "ccx63",
                "x86",
                48,
                192.0,
                960,
                Some("249.90"),
                true,
                true,
            ),
            Some(9),
        );
        let line = render_row(&r);
        assert!(line.is_ascii(), "row must be ASCII: {line}");
        assert!(
            line.chars().count() <= 64,
            "row too wide ({}): {line}",
            line.chars().count()
        );
    }

    #[test]
    fn render_row_latency_na_when_none() {
        let r = row(
            offer("nbg1", "cx22", "x86", 2, 4.0, 40, Some("3.92"), true, false),
            None,
        );
        assert!(render_row(&r).contains("n/a"));
    }

    #[test]
    fn sort_price_asc_puts_unpriced_last_and_keeps_them() {
        let mut rows = [
            row(offer("a", "x", "x86", 2, 4.0, 40, None, true, false), None), // no price
            row(
                offer("b", "y", "x86", 2, 4.0, 40, Some("10.00"), true, false),
                None,
            ),
            row(
                offer("c", "z", "x86", 2, 4.0, 40, Some("5.00"), true, false),
                None,
            ),
        ];
        sort_rows(&mut rows, SortKey::PriceAsc);
        assert_eq!(rows[0].offer.sku, "z"); // 5.00
        assert_eq!(rows[1].offer.sku, "y"); // 10.00
        assert_eq!(rows[2].offer.sku, "x"); // unpriced → last, still present
    }

    #[test]
    fn sort_latency_asc_puts_none_last() {
        let mut rows = [
            row(offer("a", "x", "x86", 2, 4.0, 40, None, true, false), None),
            row(
                offer("b", "y", "x86", 2, 4.0, 40, None, true, false),
                Some(5),
            ),
        ];
        sort_rows(&mut rows, SortKey::LatencyAsc);
        assert_eq!(rows[0].offer.sku, "y"); // 5ms first
        assert_eq!(rows[1].offer.sku, "x"); // None last
    }
}
