//! Mascot splash overlay for `rof chat`: baked ANSI art + title lines.
//!
//! The twelve sprites in `assets/mascot-N.txt` are baked to ANSI with
//! truecolor escapes. Each file's first two lines are a blank + label header
//! (e.g. `[4] laughing`); the rest is the art.
//!
//! If art from an external source is ever vendored in, name the source in
//! this comment and add its licence terms here — do not reference a
//! provenance nobody can point at.

use ansi_to_tui::IntoText;
use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

use super::theme::{AMBER, DIM};

/// Mascot expression per app state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Greet,
    Working,
    Idle,
    Explore,
    Happy,
    Error,
}

const GREET_RAW: &str = include_str!("../../assets/mascot-8.txt");
const WORKING_RAW: &str = include_str!("../../assets/mascot-5.txt");
const IDLE_RAW: &str = include_str!("../../assets/mascot-7.txt");
const EXPLORE_RAW: &str = include_str!("../../assets/mascot-10.txt");
const HAPPY_RAW: &str = include_str!("../../assets/mascot-4.txt");
const ERROR_RAW: &str = include_str!("../../assets/mascot-12.txt");

/// The full baked set: sprite files are 1-indexed (`mascot-1.txt` …
/// `mascot-12.txt`), all from the canonical cheetahs3.png sheet.
const ALL_RAW: [&str; 12] = [
    include_str!("../../assets/mascot-1.txt"),
    include_str!("../../assets/mascot-2.txt"),
    include_str!("../../assets/mascot-3.txt"),
    include_str!("../../assets/mascot-4.txt"),
    include_str!("../../assets/mascot-5.txt"),
    include_str!("../../assets/mascot-6.txt"),
    include_str!("../../assets/mascot-7.txt"),
    include_str!("../../assets/mascot-8.txt"),
    include_str!("../../assets/mascot-9.txt"),
    include_str!("../../assets/mascot-10.txt"),
    include_str!("../../assets/mascot-11.txt"),
    include_str!("../../assets/mascot-12.txt"),
];

/// Parse baked art (skipping the 2 header lines) into styled text.
/// A parse failure falls back to unstyled lines — the overlay never panics.
fn parse(raw: &str) -> Text<'static> {
    let body: String = raw.lines().skip(2).collect::<Vec<_>>().join("\n");
    body.into_text().unwrap_or_else(|_| Text::from(body))
}

/// Art for the given mood: 8 waving (greet), 5 running (working),
/// 7 sleeping (idle), 10 sniffing (explore), 4 laughing (happy),
/// 12 chirping (error).
pub fn art(mood: Mood) -> Text<'static> {
    match mood {
        Mood::Greet => parse(GREET_RAW),
        Mood::Working => parse(WORKING_RAW),
        Mood::Idle => parse(IDLE_RAW),
        Mood::Explore => parse(EXPLORE_RAW),
        Mood::Happy => parse(HAPPY_RAW),
        Mood::Error => parse(ERROR_RAW),
    }
}

/// Pick the splash sprite index for a launch, from a seed.
///
/// Pure, so the choice is made ONCE and then carried as render state. The
/// previous version called the clock inside `draw`, which meant a fresh roll
/// on every one of the pump's ~30 frames a second: the mascot flickered
/// through all twelve sprites while you were still reading the title.
pub fn pick(seed_nanos: u32) -> usize {
    seed_nanos as usize % ALL_RAW.len()
}

/// The clock the pick reads, isolated so a test can pin it.
pub fn now_nanos() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// How many sprites [`pick`] can choose from.
pub const SPRITE_COUNT: usize = ALL_RAW.len();

/// Splash art: a uniform-random sprite per launch, so every `rof chat`
/// greets you with a different cheetah. The index comes from the CALLER
/// (`App` holds it for the life of the console); no rand dependency, and
/// weighting or repetition across launches is explicitly not a goal.
pub fn art_at(index: usize) -> Text<'static> {
    parse(ALL_RAW[index % ALL_RAW.len()])
}

/// Title block under the art: "rof chat" in amber bold, the version, and a
/// dim hint. Centered per-line so the art (left-aligned) keeps its shape.
pub fn title_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "rof chat",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        Line::from(Span::styled(
            format!("v{}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(DIM),
        ))
        .alignment(Alignment::Center),
        Line::from(Span::styled(
            "/help for commands · any key to begin",
            Style::default().fg(DIM),
        ))
        .alignment(Alignment::Center),
    ]
}

/// Draw the splash overlay: art, blank separator, titles — horizontally
/// centered, vertically centered when it fits. On short screens the art is
/// cropped from the bottom so the titles (and the begin hint) stay visible.
pub fn draw(f: &mut Frame, mascot: usize) {
    let area = f.area();
    let art = art_at(mascot);
    let art_len = art.lines.len();
    let titles = title_lines();
    let reserved = titles.len() + 1; // blank separator + titles
    let max_art = (area.height as usize).saturating_sub(reserved);
    let fits = art_len <= max_art;
    let mut lines: Vec<Line<'static>> = art.lines.into_iter().take(max_art).collect();
    lines.push(Line::from(""));
    lines.extend(titles);
    // If the screen cannot even fit the titles, show their tail.
    let lines = if lines.len() > area.height as usize && !lines.is_empty() {
        let n = (area.height as usize).min(lines.len());
        lines[lines.len() - n..].to_vec()
    } else {
        lines
    };
    let want_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let h = lines.len() as u16;
    let w = want_w.min(area.width);
    let x = area.x.saturating_add(area.width.saturating_sub(w) / 2);
    // Center only when the full art fits; otherwise top-align the crop.
    let y = if fits {
        area.y.saturating_add(area.height.saturating_sub(h) / 2)
    } else {
        area.y
    };
    f.render_widget(Paragraph::new(Text::from(lines)), Rect::new(x, y, w, h));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splash_loads_enough_art_lines() {
        let moods = [
            Mood::Greet,
            Mood::Working,
            Mood::Idle,
            Mood::Explore,
            Mood::Happy,
            Mood::Error,
        ];
        for mood in moods {
            let text = art(mood);
            assert!(
                text.lines.len() >= 15,
                "{mood:?}: lines={}",
                text.lines.len()
            );
            assert!(
                text.lines[0].width() > 0,
                "{mood:?}: first art line must be non-empty"
            );
        }
        // The splash sprite is picked ONCE per console from a seed, so every
        // index in the set must be drawable: assert the whole set clears the
        // floor (no sprite is empty), not just the one this launch rolled.
        for index in 0..super::SPRITE_COUNT {
            let text = super::art_at(index);
            assert!(text.lines.len() >= 15, "sprite {index} is too short");
            assert!(text.lines[0].width() > 0, "sprite {index} is empty");
        }
    }

    #[test]
    fn the_pick_is_stable_for_a_seed_and_stays_in_range() {
        for seed in [0u32, 1, 12, 13, 999, u32::MAX] {
            let first = super::pick(seed);
            assert_eq!(first, super::pick(seed), "seed {seed} is not stable");
            assert!(
                first < super::SPRITE_COUNT,
                "seed {seed} picked out of range: {first}"
            );
        }
        // The set stays reachable: a pick that always returned one index
        // would make every launch greet identically.
        let seen: std::collections::HashSet<usize> = (0..64u32)
            .map(|n| super::pick(n.wrapping_mul(2_654_435_761)))
            .collect();
        assert!(seen.len() > 1, "the pick never varies: {seen:?}");
    }
}
