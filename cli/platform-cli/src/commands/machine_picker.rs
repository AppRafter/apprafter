// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Machine-picker helpers and interactive flow: row type, ASCII renderer, rank
//! scorer, sort keys, availability partitioning, and the `pick_machine` wizard.
//!
//! Pure sub-helpers (`prefilter`, `partition_availability`, `build_choices`) are
//! unit-tested. The interactive `pick_machine` function uses `inquire` and is
//! exercised by the real-Hetzner walk.

// The public API in this module is intentionally unused until the interactive
// picker wires it into the CLI command graph.
#![allow(dead_code)]

use std::cmp::Ordering;

use cli_core::{CliError, Result};
use cli_providers::machine::MachineOffer;
use cli_providers::machine_filter::matches_query;
use inquire::{InquireError, Select};

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

impl std::fmt::Display for SortKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            SortKey::LatencyAsc => "latency (nearest first)",
            SortKey::PriceAsc => "price (cheapest first)",
            SortKey::CoresDesc => "cores (most first)",
            SortKey::RamDesc => "RAM (most first)",
            SortKey::DiskDesc => "disk (most first)",
            SortKey::Location => "location (A→Z)",
        };
        f.write_str(label)
    }
}

// ---------------------------------------------------------------
// Pure sub-helpers (unit-tested)
// ---------------------------------------------------------------

/// Keep only rows matching an optional prefilled `region` and/or `sku` (exact
/// match, case-sensitive). Passing `None` for either field is a no-op filter on
/// that dimension. When both are `None` the input is returned unchanged.
pub fn prefilter(
    rows: Vec<MachineRow>,
    region: Option<&str>,
    sku: Option<&str>,
) -> Vec<MachineRow> {
    rows.into_iter()
        .filter(|r| {
            if let Some(reg) = region {
                if r.offer.location != reg {
                    return false;
                }
            }
            if let Some(s) = sku {
                if r.offer.sku != s {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Partition `rows` into `(available, sold_out)`, preserving relative order in
/// both partitions. `available` = `offer.available == true`.
pub fn partition_availability(rows: Vec<MachineRow>) -> (Vec<MachineRow>, Vec<MachineRow>) {
    let mut available = Vec::new();
    let mut sold_out = Vec::new();
    for r in rows {
        if r.offer.available {
            available.push(r);
        } else {
            sold_out.push(r);
        }
    }
    (available, sold_out)
}

/// Build the picker option list from the visible (available) rows.
///
/// Each element of `visible` becomes a `PickerChoice::row(...)`. When
/// `sold_out_count > 0` a `PickerChoice::ShowSoldOut` sentinel is **appended
/// last** so the user can opt into seeing the unavailable rows.
pub fn build_choices(visible: &[MachineRow], sold_out_count: usize) -> Vec<PickerChoice> {
    let mut choices: Vec<PickerChoice> = visible.iter().cloned().map(PickerChoice::row).collect();
    if sold_out_count > 0 {
        choices.push(PickerChoice::ShowSoldOut);
    }
    choices
}

// ---------------------------------------------------------------
// Interactive flow (walk-covered, not unit-tested)
// ---------------------------------------------------------------

/// Map an `inquire::InquireError` to the crate's `CliError`. Mirrors the
/// identical helper in `target_wizard.rs` — kept local to avoid cross-module
/// coupling (the function is trivially small).
fn map_inquire_err(err: InquireError) -> CliError {
    match err {
        InquireError::OperationCanceled | InquireError::OperationInterrupted => {
            CliError::Other("machine picker aborted by user".to_string())
        }
        other => CliError::Other(format!("machine picker prompt failed: {other}")),
    }
}

/// Constant for the "show sold-out" re-prompt loop guard. 32 repetitions is
/// far more than any real user would click, but avoids a true `loop {}` that
/// clippy would flag.
const MAX_REPROMPT: usize = 32;

/// Interactive (region × SKU) machine matrix. Returns the chosen
/// `(region, sku)` as strings. `prefill_region` / `prefill_sku` narrow the
/// matrix before presenting (validated upstream — `prefilter` simply drops
/// non-matching rows).
///
/// Behaviour:
/// 1. Prefilter → partition into available / sold-out.
/// 2. **Empty guard (H7):** if no available rows, return a typed
///    `CliError::Other` — never draw an empty picker.
/// 3. Sort-key pre-picker (`Select`); default `LatencyAsc`, or `PriceAsc`
///    when every visible row has `latency_ms == None`.
/// 4. Print the column legend to stderr.
/// 5. Matrix `Select` with the fuzzy scorer wired via `with_scorer`.
/// 6. If the user picks `ShowSoldOut`, re-run the matrix with sold-out rows
///    appended. Picking a sold-out row prompts again (out-of-capacity notice).
pub fn pick_machine(
    rows: Vec<MachineRow>,
    prefill_region: Option<&str>,
    prefill_sku: Option<&str>,
) -> Result<(String, String)> {
    // Step 1: prefilter + partition.
    let rows = prefilter(rows, prefill_region, prefill_sku);
    let (mut visible, sold_out) = partition_availability(rows);

    // Step 2: empty guard — never render an empty picker (no escape path for
    // the user without Ctrl-C, which maps to a cancel error anyway).
    if visible.is_empty() {
        return Err(CliError::Other(format!(
            "no available machines match the current selection ({} sold-out) \
             — try a different region or type, or retry later",
            sold_out.len()
        )));
    }

    // Step 3: sort-key pre-picker.
    let default_key = if visible.iter().all(|r| r.latency_ms.is_none()) {
        SortKey::PriceAsc
    } else {
        SortKey::LatencyAsc
    };
    let all_keys = [
        SortKey::LatencyAsc,
        SortKey::PriceAsc,
        SortKey::CoresDesc,
        SortKey::RamDesc,
        SortKey::DiskDesc,
        SortKey::Location,
    ];
    let default_idx = all_keys.iter().position(|k| *k == default_key).unwrap_or(0);
    let chosen_key = Select::new("Sort by:", all_keys.to_vec())
        .with_starting_cursor(default_idx)
        .with_help_message("↑↓ move · enter select")
        .prompt()
        .map_err(map_inquire_err)?;
    sort_rows(&mut visible, chosen_key);

    // Step 4: column legend.
    eprintln!(
        "  location  latency  sku      cores/ram/disk   arch  EUR/mo(net,excl.VAT)  \
         [*]recommended [!]retiring"
    );

    // Step 5 + 6: matrix Select with sold-out toggle loop.
    // `include_sold_out` tracks whether we are currently in "expanded" mode.
    let mut include_sold_out = false;
    for _ in 0..MAX_REPROMPT {
        // Build the option list for this iteration.
        let effective_sold_out_count = if include_sold_out { 0 } else { sold_out.len() };
        let display_rows: Vec<MachineRow> = if include_sold_out {
            visible
                .iter()
                .cloned()
                .chain(sold_out.iter().cloned())
                .collect()
        } else {
            visible.clone()
        };
        let choices = build_choices(&display_rows, effective_sold_out_count);

        // Bind the scorer. The closure captures `rows_len` by value (Copy).
        // The `Scorer<'_, PickerChoice>` type is `&'_ dyn Fn(...)` so we must
        // give the closure a name that outlives the `Select` builder. Binding
        // to a local `let scorer = &closure;` achieves exactly this.
        let rows_len = choices.len();
        let scorer_fn = move |input: &str, c: &PickerChoice, _s: &str, idx: usize| -> Option<i64> {
            score_choice(input, c, rows_len, idx)
        };
        // The `&scorer_fn` reference must live at least as long as `ans`.
        let scorer: &dyn Fn(&str, &PickerChoice, &str, usize) -> Option<i64> = &scorer_fn;

        let ans = Select::new("Machine:", choices)
            .with_scorer(&scorer)
            .with_page_size(15)
            .with_help_message("↑↓ move · type to filter · enter select")
            .prompt()
            .map_err(map_inquire_err)?;

        match ans {
            PickerChoice::ShowSoldOut => {
                // Expand to show sold-out rows and re-prompt.
                // NOTE: `with_starting_filter_input` requires a `&'a str`
                // tied to the Select's own lifetime. The in-progress filter
                // text is not exposed by inquire's return value, so we cannot
                // carry it across the toggle — re-prompt starts with an empty
                // filter. This is a known UX limitation.
                include_sold_out = true;
                eprintln!(
                    "  Showing {} sold-out row(s). Select one to check — \
                     you will be asked to pick again if unavailable.",
                    sold_out.len()
                );
            }
            PickerChoice::Row(r) => {
                if r.offer.available {
                    return Ok((r.offer.location.clone(), r.offer.sku.clone()));
                }
                // Sold-out row selected while in expanded mode.
                eprintln!(
                    "  '{}' in {} is out of capacity — pick another.",
                    r.offer.sku, r.offer.location
                );
                // Stay in expanded mode and re-prompt.
            }
        }
    }

    // Should be unreachable in practice (loop exits only when the user makes a
    // valid selection or cancels; the loop guard is a safety rail).
    Err(CliError::Other(
        "machine picker: too many sold-out re-prompts — aborting".to_string(),
    ))
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

    // ---------------------------------------------------------------
    // prefilter tests
    // ---------------------------------------------------------------

    fn make_rows() -> Vec<MachineRow> {
        vec![
            row(
                offer("hel1", "cx22", "x86", 2, 4.0, 40, Some("3.92"), true, false),
                Some(10),
            ),
            row(
                offer("nbg1", "cx22", "x86", 2, 4.0, 40, Some("3.92"), true, false),
                Some(20),
            ),
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
                Some(11),
            ),
            row(
                offer(
                    "fsn1",
                    "cx22",
                    "x86",
                    2,
                    4.0,
                    40,
                    Some("3.92"),
                    false,
                    false,
                ),
                None,
            ),
        ]
    }

    #[test]
    fn prefilter_by_region_only() {
        let rows = make_rows();
        let filtered = prefilter(rows, Some("hel1"), None);
        assert_eq!(filtered.len(), 2, "should keep both hel1 rows");
        assert!(filtered.iter().all(|r| r.offer.location == "hel1"));
    }

    #[test]
    fn prefilter_by_sku_only() {
        let rows = make_rows();
        let filtered = prefilter(rows, None, Some("cx22"));
        assert_eq!(filtered.len(), 3, "three cx22 rows across all regions");
        assert!(filtered.iter().all(|r| r.offer.sku == "cx22"));
    }

    #[test]
    fn prefilter_by_region_and_sku() {
        let rows = make_rows();
        let filtered = prefilter(rows, Some("hel1"), Some("cx22"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].offer.location, "hel1");
        assert_eq!(filtered[0].offer.sku, "cx22");
    }

    #[test]
    fn prefilter_by_neither_returns_all() {
        let rows = make_rows();
        let n = rows.len();
        let filtered = prefilter(rows, None, None);
        assert_eq!(filtered.len(), n, "no-op filter must preserve all rows");
    }

    // ---------------------------------------------------------------
    // partition_availability tests
    // ---------------------------------------------------------------

    #[test]
    fn partition_availability_splits_and_preserves_order() {
        let input = vec![
            row(
                offer("hel1", "cx22", "x86", 2, 4.0, 40, None, true, false),
                None,
            ), // available
            row(
                offer("fsn1", "cx22", "x86", 2, 4.0, 40, None, false, false),
                None,
            ), // sold out
            row(
                offer("nbg1", "ccx23", "x86", 8, 32.0, 240, None, true, false),
                Some(5),
            ), // available
            row(
                offer("hel1", "ccx33", "x86", 16, 64.0, 360, None, false, false),
                None,
            ), // sold out
        ];
        let (avail, sold) = partition_availability(input);
        assert_eq!(avail.len(), 2);
        assert_eq!(sold.len(), 2);
        // Order is preserved in each partition.
        assert_eq!(avail[0].offer.location, "hel1");
        assert_eq!(avail[1].offer.location, "nbg1");
        assert_eq!(sold[0].offer.location, "fsn1");
        assert_eq!(sold[1].offer.location, "hel1");
        // Invariant: all available rows have available==true, sold-out ==false.
        assert!(avail.iter().all(|r| r.offer.available));
        assert!(sold.iter().all(|r| !r.offer.available));
    }

    // ---------------------------------------------------------------
    // build_choices tests
    // ---------------------------------------------------------------

    #[test]
    fn build_choices_with_sold_out_appends_sentinel_last() {
        let visible = vec![
            row(
                offer("hel1", "cx22", "x86", 2, 4.0, 40, None, true, false),
                None,
            ),
            row(
                offer("nbg1", "ccx23", "x86", 8, 32.0, 240, None, true, false),
                None,
            ),
        ];
        let choices = build_choices(&visible, 3); // 3 sold-out rows

        assert_eq!(choices.len(), 3, "2 rows + 1 sentinel");
        // First two must be Row variants in order.
        assert!(matches!(&choices[0], PickerChoice::Row(r) if r.offer.sku == "cx22"));
        assert!(matches!(&choices[1], PickerChoice::Row(r) if r.offer.sku == "ccx23"));
        // Last must be the sentinel.
        assert!(
            matches!(&choices[2], PickerChoice::ShowSoldOut),
            "last element must be ShowSoldOut"
        );
    }

    #[test]
    fn build_choices_without_sold_out_has_no_sentinel() {
        let visible = vec![row(
            offer("hel1", "cx22", "x86", 2, 4.0, 40, None, true, false),
            None,
        )];
        let choices = build_choices(&visible, 0);

        assert_eq!(choices.len(), 1, "only the single row, no sentinel");
        assert!(matches!(&choices[0], PickerChoice::Row(_)));
        // Confirm there is no ShowSoldOut anywhere.
        assert!(choices
            .iter()
            .all(|c| !matches!(c, PickerChoice::ShowSoldOut)));
    }

    #[test]
    fn build_choices_preserves_row_order() {
        let visible = vec![
            row(offer("a", "z", "x86", 2, 4.0, 40, None, true, false), None),
            row(offer("b", "m", "x86", 4, 8.0, 80, None, true, false), None),
            row(
                offer("c", "a", "x86", 8, 16.0, 160, None, true, false),
                None,
            ),
        ];
        let choices = build_choices(&visible, 1);

        // 3 rows + sentinel
        assert_eq!(choices.len(), 4);
        let skus: Vec<&str> = choices
            .iter()
            .filter_map(|c| match c {
                PickerChoice::Row(r) => Some(r.offer.sku.as_str()),
                PickerChoice::ShowSoldOut => None,
            })
            .collect();
        assert_eq!(skus, vec!["z", "m", "a"]);
    }
}
