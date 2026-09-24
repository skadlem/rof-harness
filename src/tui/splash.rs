//! Mascot splash overlay for `rof chat`: baked ANSI art + title lines.
//!
//! The art is baked from the canonical cheetahs3.png sheet to
//! `assets/mascot-N.txt` (verified via `bake.py --check`).
//! Each file's first two lines are a blank + label header; the rest is art.

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

/// Splash art: a uniform-random sprite per launch, so every `rof chat`
/// greets you with a different cheetah. No rand dependency — the clock's
/// sub-second nanos pick the index; weighting or repetition across launches
/// is explicitly not a goal.
pub fn load() -> Text<'static> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    parse(ALL_RAW[nanos % ALL_RAW.len()])
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
pub fn draw(f: &mut Frame) {
    let area = f.area();
    let art = load();
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
        // `load()` is a random sprite per launch: assert it is always one
        // of the set (any sprite clears the floor, none is empty).
        for _ in 0..12 {
            let text = load();
            assert!(text.lines.len() >= 15);
            assert!(text.lines[0].width() > 0);
        }
    }
}
