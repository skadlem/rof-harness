/// Paired-task bootstrap (B resamples, fixed seed so the gate reproduces
/// without a rand dep): per-task solve-fraction pairs (a, b); resample n
/// pairs with replacement, take mean(a-b) each draw; report the observed
/// mean and the 2.5/97.5 percentiles. `None` on empty input. Unit = task
/// (rep fractions average inside a task first — never pool reps as draws).
pub fn paired_bootstrap(pairs: &[(f64, f64)], iters: u64, seed: u64) -> Option<(f64, f64, f64)> {
    let n = pairs.len();
    if n == 0 {
        return None;
    }
    let diffs: Vec<f64> = pairs.iter().map(|(a, b)| a - b).collect();
    let mean = diffs.iter().sum::<f64>() / n as f64;
    let mut state = seed | 1;
    let mut draw = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize % n
    };
    let mut means: Vec<f64> = (0..iters)
        .map(|_| {
            let s: f64 = (0..n).map(|_| diffs[draw()]).sum();
            s / n as f64
        })
        .collect();
    means.sort_by(|a, b| a.partial_cmp(b).expect("finite means"));
    let pct = |p: f64| {
        let idx = ((means.len() - 1) as f64 * p).round() as usize;
        means[idx]
    };
    Some((mean, pct(0.025), pct(0.975)))
}

/// Mean pass rate with Wilson 95% CI. Never report a bare percentage:
/// 89 tasks means wide intervals.
pub fn wilson_ci(passed: u64, total: u64) -> (f64, f64, f64) {
    if total == 0 {
        return (0.0, 0.0, 0.0);
    }
    let (p, n, z) = (passed as f64 / total as f64, total as f64, 1.96);
    let denom = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denom;
    let delta = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denom;
    (
        (p).clamp(0.0, 1.0),
        (center - delta).clamp(0.0, 1.0),
        (center + delta).clamp(0.0, 1.0),
    )
}
