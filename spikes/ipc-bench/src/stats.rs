//! Percentiles, computed the one way the whole spike uses.
//!
//! Nearest-rank on the sorted sample, so p50 and p95 are always values that
//! were actually observed and no interpolation invents a figure.

use serde::Serialize;

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Stat {
    pub p50_us: f64,
    pub p95_us: f64,
    pub max_us: f64,
}

impl Stat {
    pub fn of(values: &mut [f64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        values.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a duration"));
        Self {
            p50_us: round2(percentile(values, 0.50)),
            p95_us: round2(percentile(values, 0.95)),
            max_us: round2(values[values.len() - 1]),
        }
    }
}

/// Nearest-rank percentile of an already sorted slice.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
