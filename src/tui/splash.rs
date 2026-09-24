//! Mascot splash overlay for `rof chat`: baked ANSI art + title lines.
//!
//! The art is sprite 8 ("waving") from the canonical cheetahs3.png sheet,
//! baked to `assets/mascot-splash.txt` (verified via `bake.py --check`).
//! The file's first two lines are a label + blank header; the rest is art.

use ansi_to_tui::IntoText;
use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

use super::theme::{AMBER, DIM};

const RAW: &str = include_str!("../../assets/mascot-splash.txt");

/// Parse the baked art (skipping the 2 header lines) into styled text.
/// A parse failure falls back to unstyled lines — the overlay never panics.
pub fn load() -> Text<'static> {
    let body: String = RAW.lines().skip(2).collect::<Vec<_>>().join("\n");
    body.into_text().unwrap_or_else(|_| Text::from(body))
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
        let text = load();
        assert!(text.lines.len() >= 20, "lines={}", text.lines.len());
        assert!(
            text.lines[0].width() > 0,
            "first art line must be non-empty"
        );
    }
}
