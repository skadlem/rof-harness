use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Paragraph,
    Frame,
};

use super::{
    app::{App, DiffSnapshot, Focus, RunMode},
    theme,
};

/// Columns the workspace must have before it splits into a primary
/// transcript with the run pane beside it. A 100-column console keeps the
/// P1a vertical stack, so the row split the existing tests pin holds there.
const WIDE_MIN_COLS: u16 = 120;

/// Width of the run pane beside the transcript. 32 interior columns is
/// enough for a one-line status and leaves the transcript the rest.
const RUN_PANE_COLS: u16 = 34;

/// Rows the diff pane may take. It is the pane that gives way when the
/// terminal is short, so the transcript keeps the space either way.
const DIFF_MAX_ROWS: u16 = 6;

/// A diff pane shorter than this is a bare border with no line to read, so
/// it is dropped whole rather than drawn as a frame.
const DIFF_MIN_ROWS: u16 = 3;

/// Workspace rows the diff pane needs before it appears at all: its own
/// frame plus the transcript's 3-row floor and the run pane's share.
const DIFF_MIN_DETAIL_ROWS: u16 = 9;

/// Said when the harness has recorded no snapshot at all. Named plainly:
/// absent evidence must never read like a clean tree.
const NO_DIFF_EVIDENCE: &str = "no diff evidence yet";

/// Said when a recorded snapshot carries no changed names.
const TREE_CLEAN: &str = "tree is clean — no files changed";

/// Said under the pinned summary when the harness cut the patch at its
/// evidence bound, so the pane never presents part of a change as the
/// whole of it.
const CUT_AT_BOUND: &str = "cut at the harness evidence bound";

/// Where each pane lands. Computed from the terminal area alone, so the
/// frame never depends on what the panes are about to render.
struct Workspace {
    transcript: Rect,
    activity: Rect,
    diff: Rect,
    status: Rect,
    composer: Rect,
}

/// Rows the diff pane takes from the workspace: capped, and dropped whole
/// when the share cannot hold a line to read. The diff is the pane that
/// gives way on a short terminal, so the transcript keeps the space
/// either way.
fn diff_pane_rows(share: u16) -> u16 {
    let rows = share.min(DIFF_MAX_ROWS);
    if rows < DIFF_MIN_ROWS {
        0
    } else {
        rows
    }
}

/// Split the terminal into the five panes.
///
/// Wide splits the workspace: the transcript is primary with the run pane
/// beside it, the diff takes the lower detail area, and the status row and
/// composer stay docked at the bottom. Narrow keeps the proven P1a
/// vertical stack and gives the run and diff panes the full width.
///
/// The transcript keeps a 3-row floor, the run pane keeps its 6-row cap, and
/// the diff pane is dropped entirely on a short terminal — so no pane can
/// be squeezed into a border-only frame and the composer is never lost.
fn workspace(area: Rect) -> Workspace {
    // The detail area is everything between the docked status and composer
    // rows and the transcript's 3-row floor, which is subtracted where the
    // diff pane is sized. `room` is zero on a terminal that cannot hold the
    // diff pane and that floor together, which drops the pane whole.
    let detail = area.height.saturating_sub(6);
    let room = if detail >= DIFF_MIN_DETAIL_ROWS {
        detail
    } else {
        0
    };

    if area.width >= WIDE_MIN_COLS {
        // The diff owns the lower detail area, a third of the workspace at
        // most, so the transcript and the run pane beside it keep the rest.
        let diff_rows = diff_pane_rows(room / 3);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                // The transcript keeps its 3-row floor, exactly as the
                // narrow stack does. Sizing the upper area as
                // `detail - diff_rows` handed it zero rows on a short WIDE
                // terminal (6 rows at 160 cols), which is the one pane a
                // user cannot afford to lose: the run and diff panes give
                // way instead.
                Constraint::Min(3),
                Constraint::Length(diff_rows),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(area);
        let upper = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(3), Constraint::Length(RUN_PANE_COLS)])
            .split(rows[0]);
        return Workspace {
            transcript: upper[0],
            activity: upper[1],
            diff: rows[1],
            status: rows[2],
            composer: rows[3],
        };
    }

    // A quarter of the workspace at most: the transcript is primary, so
    // the diff is the elastic pane here and the run pane only yields when
    // the diff has taken its share. The run pane keeps the P1a rule
    // unchanged — capped at 6 rows, and yielding every one of them back to
    // the transcript on a terminal too short to spend them, which is what
    // subtracting 9 (status + composer + a 3-row transcript with its
    // border) below still means with the diff pane in between.
    let diff_rows = diff_pane_rows(room / 4);
    let activity_rows = area
        .height
        .saturating_sub(9)
        .min(6)
        .min(detail.saturating_sub(3 + diff_rows));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(activity_rows),
            Constraint::Length(diff_rows),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);
    Workspace {
        transcript: rows[0],
        activity: rows[1],
        diff: rows[2],
        status: rows[3],
        composer: rows[4],
    }
}

/// The panes this terminal size actually renders, in [`FOCUS_ORDER`]
/// (super::app::FOCUS_ORDER) order. The geometry alone decides it, so the
/// reducer that walks the focus cycle is told the same thing the frame drew:
/// a pane that was dropped whole is stepped over, never landed on.
pub fn visible_panes(area: Rect) -> Vec<Focus> {
    let panes = workspace(area);
    [
        (Focus::Transcript, panes.transcript),
        (Focus::Run, panes.activity),
        (Focus::Diff, panes.diff),
        (Focus::Composer, panes.composer),
    ]
    .into_iter()
    .filter(|(_, rect)| rect.width > 0 && rect.height > 0)
    .map(|(focus, _)| focus)
    .collect()
}

/// The lines the diff pane shows: the snapshot's own evidence, or the
/// plain name of what is missing. No git, no filesystem, no environment —
/// the snapshot is the only input, so a replayed session renders the same
/// pane.
fn diff_lines(snapshot: Option<&DiffSnapshot>, height: usize, scroll: usize) -> Vec<String> {
    let Some(snapshot) = snapshot else {
        return vec![NO_DIFF_EVIDENCE.to_string()];
    };
    if snapshot.names.is_empty() {
        return vec![TREE_CLEAN.to_string()];
    }
    // The changed paths are the pane's summary, so they are pinned above
    // the body: however long the patch is, the files it names stay
    // readable. `truncated` is the contract here — the marker string
    // inside the patch is never parsed to decide this.
    let label = if snapshot.truncated {
        "partial change"
    } else {
        "changed"
    };
    let mut lines = vec![format!(
        "{label} ({}): {}",
        snapshot.names.len(),
        snapshot.names.join(" ")
    )];
    let mut body: Vec<String> = Vec::new();
    if snapshot.truncated {
        body.push(CUT_AT_BOUND.to_string());
    }
    let stat = snapshot.stat.trim();
    if !stat.is_empty() {
        body.push(format!("stat: {stat}"));
    }
    body.extend(snapshot.patch.lines().map(str::to_string));
    // The harness bounds the patch, not the pane, so the body is tail
    // anchored and the frame never grows with the patch. The window moves
    // through the diff's OWN offset (`App::diff_scroll`); it is never the
    // transcript's `scroll`, which moved both panes at once. Each step
    // hides one more line from the bottom, clamped so the first line can
    // never scroll out of reach, which is what keeps a short patch from
    // being scrolled into blank space.
    let hidden = scroll.min(body.len().saturating_sub(1));
    let visible = body.len() - hidden;
    let window = height.saturating_sub(1);
    lines.extend(
        body.into_iter()
            .take(visible)
            .rev()
            .take(window)
            .collect::<Vec<_>>()
            .into_iter()
            .rev(),
    );
    lines
}

/// Transcript (top) · run activity · diff · status · composer (bottom, 3
/// lines). Logic-free: every string comes from `App`. No clock, git,
/// environment, or channel access happens here, so the same `&App` always
/// draws the same screen.
pub fn draw(f: &mut Frame, app: &App) {
    let panes = workspace(f.area());
    let height = panes.transcript.height.saturating_sub(2) as usize;
    let skip = app.scroll.min(app.transcript.len());
    let tail: Vec<String> = app
        .transcript
        .iter()
        .take(app.transcript.len() - skip)
        .cloned()
        .rev()
        .take(height)
        .collect();
    let shown: Vec<String> = tail.into_iter().rev().collect();
    f.render_widget(
        Paragraph::new(shown.join("\n")).block(theme::pane_or_focused(
            app.theme,
            "transcript",
            app.focus == Focus::Transcript,
        )),
        panes.transcript,
    );
    // `theme::pane` draws `Borders::ALL`, so the text area is two rows
    // shorter than the pane; asking for the full height would push the
    // newest line under the bottom border.
    let activity_inner = panes.activity.height.saturating_sub(2) as usize;
    let activity = app.activity_tail(activity_inner);
    // The empty state names the posture it is in, so a replay never claims
    // to be waiting on a live run and a settled run never implies one.
    let empty_state = if app.replay_mode {
        "replay: no live activity"
    } else {
        match app.run_mode {
            RunMode::Running | RunMode::Stopping => "waiting for run",
            RunMode::Idle | RunMode::Finished | RunMode::Failed => "no run activity",
        }
    };
    let activity: Vec<String> = if activity.is_empty() && activity_inner > 0 {
        vec![empty_state.to_string()]
    } else {
        activity
    };
    f.render_widget(
        Paragraph::new(activity.join("\n")).block(theme::pane_or_focused(
            app.theme,
            "run activity",
            app.focus == Focus::Run,
        )),
        panes.activity,
    );
    // The diff pane renders the engine's own bounded read, so it can never
    // disagree with the write gate and never runs a second `git diff`.
    let snapshot = app.diff_snapshot();
    let diff = diff_lines(
        snapshot,
        panes.diff.height.saturating_sub(2) as usize,
        app.diff_scroll,
    );
    f.render_widget(
        Paragraph::new(diff.join("\n")).block(theme::diff_block(
            app.theme,
            snapshot.is_some_and(|recorded| recorded.truncated),
            app.focus == Focus::Diff,
        )),
        panes.diff,
    );
    f.render_widget(
        Paragraph::new(app.status_line()).block(theme::pane(app.theme, "status")),
        panes.status,
    );
    let shown = if app.mask_input {
        "•".repeat(app.input.chars().count())
    } else {
        app.input.clone()
    };
    // A live run is not read-only: the composer submits steers and queues
    // the next goal, so its title names the busy mode and what is pending.
    let block = if matches!(app.run_mode, RunMode::Running | RunMode::Stopping) {
        theme::composer_block_live(
            app.theme,
            &app.thinking,
            &app.control_summary(),
            app.focus == Focus::Composer,
        )
    } else {
        theme::composer_block(app.theme, &app.thinking, app.focus == Focus::Composer)
    };
    f.render_widget(Paragraph::new(shown.as_str()).block(block), panes.composer);
}
