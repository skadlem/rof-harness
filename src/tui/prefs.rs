//! `~/.rof/tui.json`: the console's ONLY persisted state, and only the
//! non-secret UI preferences on the allowlist below.
//!
//! The allowlist is a TYPE, not a filter. A preference is a field of
//! [`Prefs`], and a value that is not a field cannot be written, read, or
//! round-tripped — there is no pass-through map to smuggle a `transcript`,
//! a `goal`, a `trace_path`, or an `api_key` through. Serde drops the
//! unknown keys of a hand-edited or hostile file on the way in, so the
//! console restores exactly the allowlist and nothing else. The fields
//! below are the whole contract: a run, a goal, a transcript, a composer
//! draft, a pending queue, a trace path, and every secret are absent from
//! this module, and that is why they cannot leak through it.
//!
//! A malformed file never prevents startup. Bad JSON, an unknown schema
//! version, a wrong-typed field: all of them fall back to the defaults
//! and the console opens as if the file were not there. A field that
//! parses but names something that does not exist (a theme, a pane) is
//! repaired to its default rather than trusted.
//!
//! Writes are atomic — a temp file in the same directory, then a rename
//! over the target — so a reader (or a crash) never sees half a file. The
//! file is written 0600 like the credential store beside it, not because
//! these values are secret but because nothing in `~/.rof` should be.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::app::{App, Focus, FOCUS_ORDER};

/// The schema version this build reads and writes. A file stamped with
/// anything else is not migrated and not partially believed: it falls back
/// to the defaults, because a layout this build does not know is not a
/// layout it can read one field of.
pub const SCHEMA_VERSION: u32 = 1;

/// Environment override for the store's location, so a test (or a user who
/// keeps `~/.rof` elsewhere) can point it at another directory. It moves
/// the FILE; it never changes what may go in it.
pub const PATH_ENV: &str = "ROF_TUI_PREFS";

/// The display posture this console renders. `Fullscreen` is the only one
/// it can actually paint: `run.rs` enters the alternate screen around the
/// whole session and leaves it on the way out, and that pairing is the P1a
/// terminal-restoration contract. A `regular` (inline) console would have
/// to give up one half of that pair to be honoured, so the mode is carried
/// in the file and validated, but `/display` reports honestly that the
/// console cannot switch — see the `Action::Display` arm in `run.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayMode {
    #[default]
    Fullscreen,
    Regular,
}

impl DisplayMode {
    pub fn name(self) -> &'static str {
        match self {
            DisplayMode::Fullscreen => "fullscreen",
            DisplayMode::Regular => "regular",
        }
    }

    /// The mode a name selects, or `None` for a name that is not a mode.
    /// The composer's `/display` parser and this are the same table, so a
    /// name the parser accepts is a name the file can hold and vice versa.
    pub fn parse(name: &str) -> Option<DisplayMode> {
        match name {
            "fullscreen" => Some(DisplayMode::Fullscreen),
            "regular" => Some(DisplayMode::Regular),
            _ => None,
        }
    }

    /// The names, for the parser's refusal.
    pub fn valid_names() -> &'static str {
        "fullscreen/regular"
    }
}

/// The persisted allowlist, in full. Every field is a non-secret UI
/// preference; every non-secret UI preference is a field. Read and written
/// through this one type, so a value the console can persist is exactly a
/// value it can restore, and nothing else has anywhere to go.
///
/// `theme` and `focus_order` are stored as the names the user typed, not
/// as resolved values, so a file stays readable and a name that does not
/// exist can be refused at load rather than at draw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    pub version: u32,
    pub theme: String,
    pub focus_order: Vec<String>,
    pub display_mode: DisplayMode,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            theme: super::theme::Theme::default().name().to_string(),
            focus_order: FOCUS_ORDER
                .iter()
                .map(|pane| pane.name().to_string())
                .collect(),
            display_mode: DisplayMode::default(),
        }
    }
}

/// The preferences a live console holds.
///
/// The only `App` state this reads is the theme, the focus order, and the
/// display posture. `transcript`, `run_goal`, `input`, `pending_steer`,
/// `pending_goal`, `replay_events`, `deferred_config`, and the counters are
/// not read here, so no code path can put them on disk even by accident:
/// they are session state, and a restart begins a new session.
pub fn from_app(app: &App) -> Prefs {
    Prefs {
        version: SCHEMA_VERSION,
        theme: app.theme.name().to_string(),
        focus_order: FOCUS_ORDER
            .iter()
            .map(|pane| pane.name().to_string())
            .collect(),
        // The one posture this console can paint. `/display` says so
        // rather than promising a switch, so the file records what the
        // console actually rendered and never a mode it ignored.
        display_mode: DisplayMode::Fullscreen,
    }
}

/// Where the preferences live: `~/.rof/tui.json`, or
/// [`PATH_ENV`] when it is set. The credential store is a DIFFERENT file
/// in the same directory and is never opened from here.
pub fn default_path() -> PathBuf {
    match std::env::var(PATH_ENV) {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path),
        _ => std::env::var("HOME")
            .map(|home| PathBuf::from(home).join(".rof/tui.json"))
            .unwrap_or_else(|_| PathBuf::from(".rof-tui.json")),
    }
}

/// Read the preferences at `path`. Total: any failure at all — absent file,
/// unreadable file, bad JSON, an unknown version, a wrong-typed field —
/// yields the defaults, and the caller proceeds exactly as it would have
/// with no file. Startup is never blocked by this file.
pub fn load(path: &Path) -> Prefs {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Prefs::default();
    };
    // Deserializing INTO the typed struct is the allowlist: a file with
    // extra keys parses (serde ignores them) but they are not part of
    // `Prefs`, so they cannot be read into the console or written back.
    let Ok(prefs) = serde_json::from_str::<Prefs>(&text) else {
        return Prefs::default();
    };
    if prefs.version != SCHEMA_VERSION {
        return Prefs::default();
    }
    Prefs {
        theme: repair_theme(&prefs.theme),
        focus_order: repair_focus_order(&prefs.focus_order),
        ..prefs
    }
}

/// A theme name that is not a theme falls back to the default rather than
/// being carried forward as a name no palette answers to.
fn repair_theme(name: &str) -> String {
    match super::theme::Theme::parse(name) {
        Some(theme) => theme.name().to_string(),
        None => super::theme::Theme::default().name().to_string(),
    }
}

/// A focus order is honoured only if it is every pane exactly once. A
/// repeated pane, a missing pane, or a name that is not a pane is a
/// different order than the console cycles, so the stored order is
/// dropped and the default stands.
fn repair_focus_order(order: &[String]) -> Vec<String> {
    if order.len() != FOCUS_ORDER.len() {
        return Prefs::default().focus_order;
    }
    let mut panes = Vec::with_capacity(order.len());
    for name in order {
        let Some(pane) = Focus::parse(name) else {
            return Prefs::default().focus_order;
        };
        if panes.contains(&pane) {
            return Prefs::default().focus_order;
        }
        panes.push(pane);
    }
    order.to_vec()
}

/// Write the preferences to `path`, replacing whatever is there.
///
/// Atomic: the body is written to a temp file in the SAME directory and
/// then renamed over the target, so the target is only ever the old
/// content or the complete new content, never a partial write. The
/// directory is created if it is missing, and the temp file is removed if
/// any step fails, so a failed write leaves nothing behind and does not
/// damage the file that was already there.
pub fn save(path: &Path, prefs: &Prefs) -> anyhow::Result<()> {
    let body = serde_json::to_string_pretty(prefs)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("tui.json"));
    let temp = parent.join(format!("{file_name}.tmp"));

    let written = (|| -> anyhow::Result<()> {
        std::fs::write(&temp, body.as_bytes())?;
        // The rename is what makes this atomic: same directory, so a
        // same-filesystem rename, and the target flips from the old
        // complete content to the new complete content in one step. No
        // mode is set on the temp file — see the module docs.
        std::fs::rename(&temp, path)?;
        Ok(())
    })();

    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}
