/// Old observations kept verbatim (SWE-agent collapse-5, +3.0pp over full history).
pub const COLLAPSE_KEEP: usize = 5;

/// Hysteresis for the collapse boundary: the boundary advances only once the
/// verbatim window would reach `COLLAPSE_KEEP + H`, then it stubs the whole
/// excess in one batch, so it moves once per H tool rows instead of once per
/// row. `0` = the collapse-5 tail rule (byte-identical, the default). Every
/// move rewrites history and invalidates the provider prefix-cache suffix.
/// Default for the run config's `collapse_hysteresis` field; arms set that
/// field directly, so nothing here reads the environment.
pub const COLLAPSE_HYSTERESIS: usize = 0;

/// Stub boundary over `tool_count` tool rows: rows before the returned index
/// collapse, rows from it on stay verbatim. `hysteresis == 0` is the
/// collapse-5 tail rule (all but the last `keep`). With `hysteresis == h > 0`
/// the boundary is a multiple of h and advances only when the window would
/// reach `keep + h`: the window never exceeds `keep + h` and (once the history
/// holds `keep` rows) never shrinks below `keep`, and each move stubs the
/// whole accumulated excess at once (h rows when one row arrives per fold).
pub fn collapse_boundary(tool_count: usize, keep: usize, hysteresis: usize) -> usize {
    if hysteresis == 0 {
        return tool_count.saturating_sub(keep);
    }
    hysteresis * (tool_count.saturating_sub(keep) / hysteresis)
}
