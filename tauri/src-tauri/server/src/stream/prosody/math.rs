//! numpy's medians, percentiles and Python's rounding, the way prosody.py
//! uses them, so the numbers come out the same.

/// `np.median`: the middle value, or the mean of the middle two. NaN for
/// none.
pub fn median(values: &[f64]) -> f64 {
    let sorted = sorted(values);
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// `np.percentile(values, q)` with linear interpolation, `q` in 0–100.
pub fn percentile(values: &[f64], q: f64) -> f64 {
    quantile_sorted(&sorted(values), q / 100.0)
}

/// `np.quantile(values, q)`, `q` in 0–1.
pub fn quantile(values: &[f64], q: f64) -> f64 {
    quantile_sorted(&sorted(values), q)
}

/// Several percentiles of the same values, sorting them once.
pub fn percentiles<const N: usize>(values: &[f64], qs: [f64; N]) -> [f64; N] {
    let sorted = sorted(values);
    qs.map(|q| quantile_sorted(&sorted, q / 100.0))
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
}

/// numpy's linear method: the virtual index `q * (n - 1)`, and its lerp
/// that counts down from the upper value past the midpoint.
fn quantile_sorted(sorted: &[f64], q: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return f64::NAN;
    }
    let virtual_index = q * (n - 1) as f64;
    let below = virtual_index.floor().clamp(0.0, (n - 1) as f64);
    let gamma = virtual_index - below;
    let low = sorted[below as usize];
    let high = sorted[(below as usize + 1).min(n - 1)];
    let difference = high - low;
    if gamma >= 0.5 {
        high - difference * (1.0 - gamma)
    } else {
        low + difference * gamma
    }
}

/// Python's `round(value, 2)`: the decimal nearest the exact binary value,
/// ties to even.
pub fn round2(value: f64) -> f64 {
    round_to(value, 2)
}

/// Python's `round(value, digits)`.
pub fn round_to(value: f64, digits: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    format!("{value:.digits$}").parse().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians_and_percentiles_match_numpy() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
        assert!(median(&[]).is_nan());
        let values: Vec<f64> = (0..11).map(f64::from).collect();
        assert_eq!(percentile(&values, 10.0), 1.0);
        assert_eq!(percentile(&values, 90.0), 9.0);
        // np.percentile([1, 2, 4, 8], [10, 50, 90]) == [1.3, 3.0, 6.8]
        let [low, mid, high] = percentiles(&[1.0, 2.0, 4.0, 8.0], [10.0, 50.0, 90.0]);
        assert!((low - 1.3).abs() < 1e-12 && mid == 3.0 && (high - 6.8).abs() < 1e-12);
        assert_eq!(quantile(&[5.0], 0.98), 5.0);
    }

    #[test]
    fn rounding_is_pythons() {
        assert_eq!(round2(2.675), 2.67);
        assert_eq!(round2(0.125), 0.12);
        assert_eq!(round2(0.375), 0.38);
        assert_eq!(round2(-1.005), -1.0);
        assert_eq!(round_to(0.15625, 3), 0.156);
    }
}
