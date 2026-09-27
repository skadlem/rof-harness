//! Shared TUI palette + pane frames.
//!
//! Named ANSI colors only, so the UI respects the user's terminal theme.
//! Single-accent scheme: the accent leads (titles, composer), a dim color
//! frames the panes, the highlight color is reserved for focus, and
//! green/red only for pass/fail.
//!
//! This module owns the palette AND the frames. Every color a pane draws
//! is resolved here from a [`Theme`], which `ui.rs` forwards from `App` as
//! an argument: the renderer never names a color itself, and a frame
//! constructor cannot be called without saying which theme it is drawing.
//! [`Theme`] is `Copy` and holds nothing but plain `Color` values, so
//! resolving it per frame costs a few words, and it reads no env, no clock,
//! and no filesystem — a frame stays a pure function of `App`.

use ratatui::{
    style::{Color, Style},
    widgets::{Block, Borders},
};

/// The terminal-default palette, kept as named constants because it is what
/// the pre-theme console drew, byte for byte: a user who never runs
/// `/theme` must see no change at all.
pub const AMBER: Color = Color::Yellow;
pub const CYAN: Color = Color::Cyan;
pub const DIM: Color = Color::DarkGray;
pub const PASS: Color = Color::Green;
pub const FAIL: Color = Color::Red;

/// The colors one theme resolves to. Every frame here is built from these
/// five, so a theme that forgot a color would have to be spelled out here
/// rather than drifting pane to pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// Pane titles, and the composer's border.
    pub accent: Color,
    /// The focus indicator: the border of the pane the keys act on.
    pub highlight: Color,
    /// The frame of every unfocused pane.
    pub dim: Color,
    /// Pass/fail only, never decoration.
    pub pass: Color,
    pub fail: Color,
}

/// A built-in theme: the whole palette, chosen by name from the composer.
///
/// `Default` is the terminal-native scheme and is deliberately the derived
/// default for [`crate::tui::app::App`], so a console that never runs
/// `/theme` renders exactly what it rendered before themes existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Named ANSI colors, so the console matches whatever the terminal
    /// already is. The compatibility palette.
    #[default]
    Default,
    /// Brighter, higher-contrast set for a dark background.
    Dark,
    /// Dark inks for a light background.
    Light,
}

impl Theme {
    /// Every theme `/theme` will name, in listing order. The parser, the
    /// listing, and the refusal all read this one list, so they cannot
    /// disagree about what exists.
    pub const ALL: [Theme; 3] = [Theme::Default, Theme::Dark, Theme::Light];

    /// The word a user types and reads back.
    pub fn name(self) -> &'static str {
        match self {
            Theme::Default => "default",
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }

    /// The theme a name selects, or `None` for a name that is not a theme.
    /// There is no fallback here on purpose: a caller that gets `None` must
    /// refuse the name, never quietly land on [`Theme::Default`].
    pub fn parse(name: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The names, in the form the parser's refusals and `/help` use.
    pub fn valid_names() -> String {
        Theme::ALL
            .iter()
            .map(|t| t.name())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// What `/theme` with no argument writes: every choice, and which one
    /// is in force. Both halves are the user's to need — the list to pick
    /// from, the marker to know where they are.
    pub fn listing(current: Theme) -> String {
        let names: Vec<String> = Theme::ALL
            .iter()
            .map(|t| {
                if *t == current {
                    format!("{} (current)", t.name())
                } else {
                    t.name().to_string()
                }
            })
            .collect();
        format!("theme: {}", names.join(", "))
    }

    /// The refusal for a name that is not a theme. It says what was typed
    /// and what is valid, because the user's next move is to retype.
    pub fn unknown_name_line(got: &str) -> String {
        format!("/theme takes {} (got {got})", Theme::valid_names())
    }

    /// The confirmation after a switch, so the transcript names the theme
    /// instead of leaving the repaint to be guessed at.
    pub fn applied_line(self) -> String {
        format!("theme={}: every pane follows it", self.name())
    }

    /// This theme's colors. The one place a `Color` is chosen.
    pub fn palette(self) -> Palette {
        match self {
            // The pre-theme console, exactly.
            Theme::Default => Palette {
                accent: AMBER,
                highlight: CYAN,
                dim: DIM,
                pass: PASS,
                fail: FAIL,
            },
            Theme::Dark => Palette {
                accent: Color::LightYellow,
                highlight: Color::LightCyan,
                dim: Color::Gray,
                pass: Color::LightGreen,
                fail: Color::LightRed,
            },
            Theme::Light => Palette {
                accent: Color::Blue,
                highlight: Color::Magenta,
                dim: Color::DarkGray,
                pass: Color::Green,
                fail: Color::Red,
            },
        }
    }
}

/// Titled bordered pane with a dim border, in `theme`'s frame color.
pub fn pane(theme: Theme, title: &str) -> Block<'_> {
    let palette = theme.palette();
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.dim))
}

/// Appended to the title of the pane the keys act on. It is APPENDED, never
/// prefixed, so the frame's own title is still the first thing read, and it
/// carries no word that could read as a pane the user is barred from — the
/// composer is focused by default, so a "read-only" mark there would be a
/// lie the console cannot act on.
pub const FOCUS_MARK: &str = " ▸";

/// Titled bordered pane holding the focus: the same frame, the mark on its
/// title, and the highlight accent cyan already reserves. No new color is
/// introduced for focus — the scheme says cyan is for highlights, and a
/// focused pane is one.
pub fn focused_pane(theme: Theme, title: &str) -> Block<'_> {
    let palette = theme.palette();
    Block::default()
        .title(format!("{title}{FOCUS_MARK}"))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.highlight))
}

/// The dim pane, or its focused frame, according to where the keys go.
pub fn pane_or_focused(theme: Theme, title: &str, focused: bool) -> Block<'_> {
    if focused {
        focused_pane(theme, title)
    } else {
        pane(theme, title)
    }
}

/// Composer frame: amber border. The title carries the thinking state so the
/// state-color intent stays a single accent for now. A live run uses
/// [`composer_block_live`] instead, which names the busy mode and whatever
/// is pending.
///
/// The P1a `read_only` variant of this title is GONE, not merely unused: a
/// live composer submits steers and queued goals, so nothing can render that
/// claim truthfully any more. `focused` only ADDS the focus mark: the
/// composer keeps its amber border whether or not it holds the focus, so
/// being focused never changes what the composer looks like it can do.
pub fn composer_block(theme: Theme, thinking: &str, focused: bool) -> Block<'static> {
    let title = if thinking.trim().is_empty() {
        "composer".to_string()
    } else {
        format!("composer · {}", thinking.trim())
    };
    composer_title(theme, title, focused)
}

/// The composer's own title, with the focus mark when it holds the focus.
fn composer_title(theme: Theme, title: String, focused: bool) -> Block<'static> {
    let title = if focused {
        format!("{title}{FOCUS_MARK}")
    } else {
        title
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.palette().accent))
}

/// Diff frame. A snapshot the harness cut at its evidence bound gets a
/// `partial` title, so a short patch can never read as the whole change;
/// the body carries the same claim in words. Every other posture uses
/// [`pane`], which this defers to, or [`focused_pane`] when the diff holds
/// the focus.
pub fn diff_block(theme: Theme, truncated: bool, focused: bool) -> Block<'static> {
    let title = if truncated { "diff (partial)" } else { "diff" };
    pane_or_focused(theme, title, focused)
}

/// Longest control summary a live composer title may carry. The title
/// rides the top border of a 3-row pane, so a longer summary is cut with
/// an ellipsis: the title is one line by construction and can neither wrap
/// onto a second row nor widen the frame.
const LIVE_TITLE_SUMMARY_MAX_CHARS: usize = 40;

/// Longest thinking label a live composer title may carry. The thinking
/// label comes from `ROF_THINKING` and is unbounded, so it is the segment
/// that gets cut: the control state is what the user needs to read, and a
/// long thinking label must not push it off the top border.
const LIVE_TITLE_THINKING_MAX_CHARS: usize = 20;

/// Truncate `text` to `max` characters, ellipsis included when anything was
/// dropped. Char-based, so a multi-byte label is never split mid-codepoint.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max - 1).collect();
    format!("{kept}…")
}

/// Composer frame for a live run. P1a is over: a live composer submits
/// steers and queued goals, so the title names what the composer is doing —
/// the thinking posture plus the busy mode, occupied slots, and deferred
/// settings from [`App::control_summary`](crate::tui::app::App::control_summary).
/// Each segment is bounded so the whole title stays one line, and the
/// control segment is what survives a long thinking label.
///
/// Every other posture uses [`composer_block`].
pub fn composer_block_live(
    theme: Theme,
    thinking: &str,
    control: &str,
    focused: bool,
) -> Block<'static> {
    let mut title = String::from("composer");
    let thinking = thinking.trim();
    if !thinking.is_empty() {
        title.push_str(" · ");
        title.push_str(&clip(thinking, LIVE_TITLE_THINKING_MAX_CHARS));
    }
    let control = control.trim();
    if !control.is_empty() {
        title.push_str(" · ");
        title.push_str(&clip(control, LIVE_TITLE_SUMMARY_MAX_CHARS));
    }
    // The focus mark is the last segment, so the control state keeps its
    // place in the budget and only the mark is what a narrow title can lose.
    composer_title(theme, title, focused)
}
