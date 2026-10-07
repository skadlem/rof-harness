//! Output condenser: drop build noise, keep signal lines, cap kept lines.
//!
//! The keep-list is substring-based (failures, errors, panics, `test result:`
//! summaries) so a verdict buried in a noise flood still surfaces; anything
//! unmatched falls back to the raw tail. Pure build logic, no process or
//! policy code, so it lives next to its callers.
// ponytail: hand-rolled rank-and-cap instead of a truncation helper crate.

use std::collections::HashSet;

fn is_noise(t: &str) -> bool {
    t.is_empty()
        || t.starts_with("Compiling")
        || t.starts_with("Finished")
        || t.starts_with("Running")
        || t.starts_with("Doc-tests")
        || t.starts_with("Blocking")
        || (t.starts_with("test ") && t.ends_with("... ok"))
}

const TAIL_LINES: usize = 12;
const TAIL_BYTES: usize = 2000;

/// Keep-list matched nothing: a bare Python traceback, panic backtrace or
/// timeout notice carries no signal substring, only a tail. Keep the last
/// `TAIL_LINES` raw lines within `TAIL_BYTES`; a pure-noise tail (all
/// `is_noise`) keeps the old empty-result sentence so green build logs stay
/// as terse as before.
fn condense_tail(s: &str) -> String {
    let all: Vec<&str> = s.lines().collect();
    if all.is_empty() {
        return "(no actionable lines in 0 lines of output)".to_string();
    }
    let mut first = all.len();
    let mut bytes = 0usize;
    while first > 0 && all.len() - first < TAIL_LINES {
        let cost = all[first - 1].len() + 1;
        if bytes + cost > TAIL_BYTES {
            break;
        }
        bytes += cost;
        first -= 1;
    }
    let mut body = if first == all.len() {
        // Even the last line alone exceeds the budget: keep its tail bytes.
        first -= 1;
        let line = all[first];
        let mut from = line.len().saturating_sub(TAIL_BYTES);
        while !line.is_char_boundary(from) {
            from += 1;
        }
        line[from..].to_string()
    } else {
        all[first..].join("\n")
    };
    if !all[first..].iter().any(|l| !is_noise(l.trim())) {
        return format!("(no actionable lines in {} lines of output)", all.len());
    }
    if first > 0 {
        body.push_str(&format!("\n[...{first} earlier lines omitted]"));
    }
    body
}

/// Drop build noise, keep signal substrings, cap line count. With a
/// baseline (`Some` pre-existing output), kept lines already present in
/// the baseline are dropped as noise so only new signal surfaces. When
/// the keep-list matches nothing at all, [`condense_tail`] returns the raw
/// tail instead of the empty-result sentence; baseline suppression alone
/// keeps that sentence.
pub fn condense_output(s: &str, baseline: Option<&str>) -> String {
    const KEEP: [&str; 9] = [
        "FAILED",
        "error",
        "panicked",
        "assertion",
        "warning",
        "test result",
        "failures:",
        "left:",
        "right:",
    ];
    let base: Option<HashSet<&str>> = baseline.map(|b| b.lines().collect());
    let mut kept: Vec<&str> = Vec::new();
    let mut matched = 0usize;
    let mut dropped = 0usize;
    for line in s.lines() {
        let t = line.trim();
        if is_noise(t) {
            dropped += 1;
            continue;
        }
        if KEEP.iter().any(|k| t.contains(k)) {
            matched += 1;
            if base.as_ref().is_some_and(|bs| bs.contains(line)) {
                dropped += 1;
                continue;
            }
            kept.push(line);
        } else {
            dropped += 1;
        }
    }
    if kept.is_empty() && matched == 0 {
        return condense_tail(s);
    }
    if kept.is_empty() {
        return format!(
            "(no actionable lines in {} lines of output)",
            s.lines().count()
        );
    }
    let omitted = kept.len().saturating_sub(80);
    let body = if omitted > 0 {
        pick_kept(&kept).join("\n")
    } else {
        kept.join("\n")
    };
    if omitted > 0 || dropped > 0 {
        format!("{body}\n[...{dropped} noise lines and {omitted} kept-lines over cap omitted]")
    } else {
        body
    }
}

/// Exit-(c) budget: exactly 80 of the matched lines. The matched set's last
/// line is always kept (a pytest `test result:` summary sits there), and the
/// remaining 79 rank specific signal substrings (`FAILED`, panics, assertions,
/// pytest summaries) ahead of the high-volume `error`/`warning` matches. Each
/// class contributes its own head and tail, so a verdict in the middle of a
/// message flood survives instead of being sliced off.
fn pick_kept<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    const BUDGET: usize = 80;
    let last = lines.len() - 1;
    let (mut specific, mut generic) = (Vec::new(), Vec::new());
    for (i, l) in lines[..last].iter().enumerate() {
        if is_specific(l.trim()) {
            specific.push(i);
        } else {
            generic.push(i);
        }
    }
    let mut picked = vec![last];
    if specific.len() >= BUDGET - 1 {
        picked.extend(head_tail(&specific, BUDGET - 1));
    } else {
        picked.extend(&specific);
        picked.extend(head_tail(&generic, BUDGET - 1 - specific.len()));
    }
    picked.sort_unstable();
    picked.into_iter().map(|i| lines[i]).collect()
}

/// First and last `budget` entries of `idx`, extra slot on the head side.
fn head_tail(idx: &[usize], budget: usize) -> Vec<usize> {
    if idx.len() <= budget {
        return idx.to_vec();
    }
    let head = budget.div_ceil(2);
    let mut out = idx[..head].to_vec();
    out.extend_from_slice(&idx[idx.len() - (budget - head)..]);
    out
}

fn is_specific(t: &str) -> bool {
    [
        "FAILED",
        "panicked",
        "assertion",
        "test result",
        "failures:",
        "left:",
        "right:",
    ]
    .iter()
    .any(|k| t.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condense_keeps_signal_drops_noise_and_caps() {
        let mut raw = String::from("test a ... ok\ntest b ... FAILED\ntest result: FAILED\n");
        for i in 0..100 {
            raw.push_str(&format!("error line {i}\n"));
        }
        let out = condense_output(&raw, None);
        assert!(out.contains("test b ... FAILED"));
        assert!(!out.contains("test a ... ok"));
        assert!(out.contains("over cap omitted"));
        assert_eq!(
            out.lines().filter(|l| l.contains("error line")).count(),
            80 - 2
        );
        let green = condense_output("   Compiling rof v0.1.0\n    Finished dev profile\n", None);
        assert!(green.contains("no actionable lines"));
    }

    #[test]
    fn condense_over_cap_keeps_tail_verdict_line() {
        // The old tail-biased `kept[..80]` slice dropped the pytest summary
        // when a flood of warnings preceded it; the cap must keep the tail.
        let mut raw = String::new();
        for i in 0..85 {
            raw.push_str(&format!("warning: unused variable var_{i}\n"));
        }
        raw.push_str("test result: FAILED. 3 failed, 97 passed in 1.20s\n");
        let out = condense_output(&raw, None);
        assert!(out.contains("test result: FAILED. 3 failed"), "{out}");
        assert!(out.contains("warning: unused variable var_0"), "{out}");
        assert!(out.contains("warning: unused variable var_84"), "{out}");
        assert!(
            out.ends_with("[...0 noise lines and 6 kept-lines over cap omitted]"),
            "{out}"
        );
        assert_eq!(out.lines().count(), 81, "{out}");
    }

    #[test]
    fn condense_over_cap_ranks_specific_mid_set_line() {
        // Specific tokens outrank error/warning floods: a panic at matched
        // index 95 survives although both the old `kept[..80]` slice and a
        // plain first-40/last-40 window would drop it.
        let mut raw = String::new();
        for i in 0..95 {
            raw.push_str(&format!("warning: early {i}\n"));
        }
        raw.push_str("thread 'main' panicked at src/lib.rs:1\n");
        for i in 0..100 {
            raw.push_str(&format!("warning: late {i}\n"));
        }
        let out = condense_output(&raw, None);
        assert!(out.contains("panicked at src/lib.rs:1"), "{out}");
        assert!(out.contains("warning: early 0"), "{out}");
        assert!(out.contains("warning: late 99"), "{out}");
        assert_eq!(
            out.lines().filter(|l| l.starts_with("warning")).count(),
            79,
            "{out}"
        );
    }

    #[test]
    fn condense_baseline_reports_only_new_lines() {
        let raw = "error old\nerror new\ntest result: FAILED\n";
        let without = condense_output(raw, None);
        assert!(without.contains("error old"));
        assert!(without.contains("error new"));
        let with = condense_output(raw, Some("error old\n"));
        assert!(!with.contains("error old"), "{with}");
        assert!(with.contains("error new"), "{with}");
        let all_old = condense_output("error old\n", Some("error old\n"));
        assert!(all_old.contains("no actionable lines"), "{all_old}");
    }

    #[test]
    fn condense_keeps_bare_traceback_tail() {
        // No KEEP substring anywhere ("ValueError" is not "error"): the only
        // actionable line sits past the keep-list's reach, at the end.
        let mut raw = String::from("Traceback (most recent call last):\n");
        for i in 0..40 {
            raw.push_str(&format!("  File frame_{i}.rs, in <module>\n"));
        }
        raw.push_str("ValueError: boom\n");
        let out = condense_output(&raw, None);
        assert!(out.contains("ValueError: boom"), "{out}");
        assert!(out.contains("earlier lines omitted"), "{out}");
        assert!(!out.contains("no actionable lines"), "{out}");
        assert!(out.lines().count() <= TAIL_LINES + 1, "{out}");
        // The byte budget holds even for one huge last line.
        let huge = format!("Traceback:\n{}\n", "z".repeat(10_000));
        let cut = condense_output(&huge, None);
        assert_eq!(cut.lines().next().unwrap().len(), TAIL_BYTES, "{cut}");
        // Pure-noise tail keeps the terse sentence (green build logs).
        let green = condense_output("   Compiling a\n    Finished dev\n", None);
        assert!(green.contains("no actionable lines"), "{green}");
    }

    #[test]
    fn condense_unchanged_when_keep_list_matches() {
        // Non-empty keep-list: byte-identical to the pre-fallback rendering.
        let raw = "   Compiling x\nerror: E0308\nfiller a\nfiller b\n";
        assert_eq!(
            condense_output(raw, None),
            "error: E0308\n[...3 noise lines and 0 kept-lines over cap omitted]"
        );
        assert_eq!(
            condense_output(raw, Some("error: E0308\n")),
            "(no actionable lines in 4 lines of output)"
        );
    }
}
