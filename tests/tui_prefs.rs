//! `~/.rof/tui.json`: the console's ONLY persisted state, and only the
//! non-secret UI preferences on the allowlist.
//!
//! Headless throughout: every test points the store at its own temp
//! directory, so nothing here reads or writes the real `~/.rof`, and no
//! test touches the credential store. The allowlist is a TYPE, not a
//! filter, so a secret-shaped field in the file is not merely dropped on
//! save — it has nowhere to live in `Prefs` in the first place.

use std::path::PathBuf;

use rof::tui::app::App;
use rof::tui::prefs::{self, Prefs, SCHEMA_VERSION};
use rof::tui::run::app_with_prefs;
use rof::tui::theme::Theme;

/// A fresh, uniquely named temp directory per test. Removed on drop, so a
/// failing assertion cannot leave a preferences file behind for the next run.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rof-prefs-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp sandbox");
        Self { dir }
    }

    /// The file inside the sandbox, whose parent does not exist yet: the
    /// store has to create it.
    fn file(&self) -> PathBuf {
        self.dir.join(".rof/tui.json")
    }

    fn write_raw(&self, body: &str) -> PathBuf {
        let path = self.file();
        std::fs::create_dir_all(path.parent().unwrap()).expect("sandbox .rof");
        std::fs::write(&path, body).expect("write raw prefs");
        path
    }

    /// Every file in the sandbox directory that holds the store, sorted.
    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
            .expect("read sandbox")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn read(&self) -> String {
        std::fs::read_to_string(self.file()).expect("read prefs")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Restore write permission first: a test that made the directory
        // read-only must still be able to clean up after itself.
        let _ = std::fs::set_permissions(&self.dir, permissions(0o755));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(unix)]
fn permissions(mode: u32) -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    std::fs::Permissions::from_mode(mode)
}

#[cfg(not(unix))]
fn permissions(_mode: u32) -> std::fs::Permissions {
    std::fs::Permissions::from_std(
        #[allow(clippy::permissions_set_readonly_false)]
        { std::fs::File::open("/dev/null").expect("open /dev/null") }
            .metadata()
            .expect("stat /dev/null")
            .permissions(),
    )
}

fn rotate_focus_order() -> Vec<String> {
    vec![
        "composer".to_string(),
        "transcript".to_string(),
        "run".to_string(),
        "diff".to_string(),
    ]
}

/// The allowlist is exactly these four keys, so the file's shape is
/// assertable rather than assumed. Sorted, because that is the order
/// `keys_of` compares in.
const ALLOWLIST: [&str; 4] = ["display_mode", "focus_order", "theme", "version"];

fn keys_of(body: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(body).expect("prefs file is JSON");
    let mut keys: Vec<String> = value
        .as_object()
        .expect("prefs file is an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// A valid file round-trips every allowlisted field: what a console wrote
/// is exactly what the next one reads.
#[test]
fn a_valid_file_round_trips_every_allowlisted_field() {
    let sandbox = Sandbox::new("round-trip");
    let written = Prefs {
        version: SCHEMA_VERSION,
        theme: Theme::Dark.name().to_string(),
        focus_order: rotate_focus_order(),
        display_mode: prefs::DisplayMode::Regular,
    };
    prefs::save(&sandbox.file(), &written).expect("save prefs");

    let read = prefs::load(&sandbox.file());
    assert_eq!(read, written, "the allowlisted fields did not round trip");
    assert_eq!(
        keys_of(&sandbox.read()),
        ALLOWLIST,
        "the file carries something outside the allowlist"
    );
}

/// The allowlist is the whole file: nothing else is written, so a value
/// the console never asked to store has no key to appear under.
#[test]
fn the_written_file_holds_exactly_the_allowlisted_keys() {
    let sandbox = Sandbox::new("allowlist-only");
    let app = App::new();
    prefs::save(&sandbox.file(), &prefs::from_app(&app)).expect("save prefs");
    assert_eq!(keys_of(&sandbox.read()), ALLOWLIST);
}

// --- malformed input: never prevents startup, always falls back --------

/// A file that is not JSON at all is ignored whole; the console starts on
/// the defaults.
#[test]
fn bad_json_falls_back_to_defaults_and_startup_continues() {
    let sandbox = Sandbox::new("bad-json");
    let path = sandbox.write_raw("{ \"version\": 1, \"theme\": ");

    let read = prefs::load(&path);
    assert_eq!(read, Prefs::default(), "a partial file was half-read");

    let app = app_with_prefs(&path);
    assert_eq!(app.theme, Theme::default());
    assert_eq!(app.run_mode, rof::tui::app::RunMode::Idle);
    assert!(app.transcript.is_empty());
}

/// A schema version this build does not know is not migrated and not
/// guessed at: defaults.
#[test]
fn a_wrong_schema_version_falls_back_to_defaults() {
    let sandbox = Sandbox::new("wrong-version");
    let body = format!(
        r#"{{"version":{},"theme":"dark","focus_order":{},"display_mode":"regular"}}"#,
        SCHEMA_VERSION + 41,
        serde_json::to_string(&rotate_focus_order()).unwrap()
    );
    let path = sandbox.write_raw(&body);

    let read = prefs::load(&path);
    assert_eq!(read.version, SCHEMA_VERSION, "the version was not repaired");
    assert_eq!(read.theme, Theme::default().name());
    assert_eq!(read.display_mode, prefs::DisplayMode::Fullscreen);
    assert_eq!(app_with_prefs(&path).theme, Theme::default());
}

/// A theme name that is not a theme is repaired to the default; the rest
/// of the file is still honoured, because one bad name is not a reason to
/// throw away the user's whole layout.
#[test]
fn an_unknown_theme_name_falls_back_to_the_default_theme() {
    let sandbox = Sandbox::new("unknown-theme");
    let body = format!(
        r#"{{"version":{v},"theme":"neon-hotdog","focus_order":{order},"display_mode":"regular"}}"#,
        v = SCHEMA_VERSION,
        order = serde_json::to_string(&rotate_focus_order()).unwrap()
    );
    let path = sandbox.write_raw(&body);

    let read = prefs::load(&path);
    assert_eq!(
        read.theme,
        Theme::default().name(),
        "a bogus theme survived"
    );
    assert_eq!(
        read.focus_order,
        rotate_focus_order(),
        "the rest was dropped"
    );
    assert_eq!(app_with_prefs(&path).theme, Theme::default());
}

/// A field of the wrong type fails the typed read, so the file falls back
/// to defaults rather than half-applying.
#[test]
fn a_wrong_typed_field_falls_back_to_defaults() {
    let sandbox = Sandbox::new("wrong-type");
    let body = format!(
        r#"{{"version":{v},"theme":42,"focus_order":"composer","display_mode":"regular"}}"#,
        v = SCHEMA_VERSION
    );
    let path = sandbox.write_raw(&body);

    assert_eq!(prefs::load(&path), Prefs::default());
    let app = app_with_prefs(&path);
    assert_eq!(app.theme, Theme::default());
    assert_eq!(app.run_mode, rof::tui::app::RunMode::Idle);
}

// --- the allowlist is structural, not a filter -------------------------

/// A hostile file — one carrying a key, a transcript, a goal, a trace
/// path, a run, and a queue — is neither read into the console nor written
/// back out. The allowlist is a struct, so these names are not fields the
/// store can hold, and serde drops them on the way in.
#[test]
fn a_hostile_file_neither_restores_nor_rewrites_its_extra_fields() {
    let sandbox = Sandbox::new("hostile");
    let path = sandbox.write_raw(
        r#"{"version":1,"theme":"dark","focus_order":["composer","transcript","run","diff"],
            "display_mode":"regular","api_key":"sk-live-DO-NOT-PERSIST","transcript":["secret goal"],
            "goal":"take over the machine","trace_path":"/tmp/trace.jsonl","run_mode":"running",
            "queue":["next goal"],"pending_control":{"id":7,"text":"steer"}}"#,
    );

    let read = prefs::load(&path);
    assert_eq!(
        read.theme,
        Theme::Dark.name(),
        "the valid theme was not read"
    );
    assert_eq!(read.focus_order, rotate_focus_order());
    assert_eq!(read.display_mode, prefs::DisplayMode::Regular);

    // The typed value has no room for the extras, so re-saving cannot
    // resurrect them. (`transcript` is checked by the key set above, not
    // here: it is also a pane name, so it legitimately appears inside the
    // allowlisted focus order.)
    prefs::save(&path, &read).expect("re-save prefs");
    let body = sandbox.read();
    assert_eq!(keys_of(&body), ALLOWLIST, "an extra field was written back");
    for leak in [
        "sk-live-DO-NOT-PERSIST",
        "api_key",
        "goal",
        "trace_path",
        "run_mode",
        "queue",
        "pending_control",
    ] {
        assert!(!body.contains(leak), "{leak} survived the round trip");
    }

    // And a restarted console restored none of it.
    let app = app_with_prefs(&path);
    assert_eq!(app.run_mode, rof::tui::app::RunMode::Idle);
    assert!(app.run_goal.is_empty());
    assert!(app.transcript.is_empty());
    assert!(app.pending_steer.is_none());
    assert!(app.pending_goal.is_none());
}

/// A focus order naming a pane that does not exist — or repeating one — is
/// not a focus order. The stored order is repaired, not trusted.
#[test]
fn a_hostile_focus_order_cannot_inject_a_pane() {
    for (tag, order) in [
        ("inject", r#"["api_key","transcript","run","diff"]"#),
        ("dup", r#"["composer","composer","run","diff"]"#),
        ("short", r#"["composer"]"#),
        ("empty", "[]"),
    ] {
        let sandbox = Sandbox::new(&format!("focus-{tag}"));
        let body = format!(
            r#"{{"version":{v},"theme":"dark","focus_order":{order},"display_mode":"fullscreen"}}"#,
            v = SCHEMA_VERSION
        );
        let path = sandbox.write_raw(&body);

        let read = prefs::load(&path);
        assert_eq!(
            read.focus_order,
            Prefs::default().focus_order,
            "an invalid focus order survived ({tag})"
        );
    }
}

// --- the write is atomic ----------------------------------------------

/// The replacement is a rename, so the target is never a half-written
/// file, and the temp file it was written through does not survive.
#[test]
fn the_write_replaces_atomically_and_leaves_no_temp_file() {
    let sandbox = Sandbox::new("atomic");
    let path = sandbox.file();
    let dark = Prefs {
        version: SCHEMA_VERSION,
        theme: Theme::Dark.name().to_string(),
        focus_order: rotate_focus_order(),
        display_mode: prefs::DisplayMode::Regular,
    };
    let light = Prefs {
        theme: Theme::Light.name().to_string(),
        ..dark.clone()
    };
    prefs::save(&path, &dark).expect("first save");
    prefs::save(&path, &light).expect("second save");

    assert_eq!(
        prefs::load(&path),
        light,
        "the target is not the new content"
    );
    assert_eq!(
        sandbox.files(),
        vec![".rof".to_string()],
        "a temp file was left behind"
    );
}

/// When the write cannot complete, the previous file is still whole and
/// still readable, and nothing partial is left in its place.
#[test]
fn a_failed_write_leaves_the_previous_file_intact() {
    let sandbox = Sandbox::new("failed-write");
    let path = sandbox.file();
    let good = Prefs {
        version: SCHEMA_VERSION,
        theme: Theme::Dark.name().to_string(),
        focus_order: rotate_focus_order(),
        display_mode: prefs::DisplayMode::Regular,
    };
    let parent = path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&parent).expect("sandbox .rof");
    prefs::save(&path, &good).expect("first save");
    let before = sandbox.read();

    // A read-only parent makes the temp write fail. Probe whether the mode
    // is actually enforced on this host (a privileged user ignores it), so
    // the test asserts a real failure where one is available instead of
    // silently passing without exercising the branch.
    let parent = path.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&parent, permissions(0o500)).expect("lock the parent");
    let enforced = std::fs::write(parent.join("probe"), b"probe").is_err();
    let mut other = good.clone();
    other.theme = Theme::Light.name().to_string();
    let result = prefs::save(&path, &other);
    let _ = std::fs::remove_file(parent.join("probe"));
    let _ = std::fs::set_permissions(&parent, permissions(0o755));

    if enforced {
        assert!(result.is_err(), "a read-only parent did not stop the write");
        assert_eq!(
            sandbox.read(),
            before,
            "a failed write damaged the previous file"
        );
        assert_eq!(
            prefs::load(&path),
            good,
            "the previous preferences are no longer readable"
        );
        let names = std::fs::read_dir(&parent)
            .expect("read parent")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["tui.json".to_string()], "a partial file stayed");
    }
    assert_eq!(
        prefs::load(&path),
        good,
        "the store is unreadable afterwards"
    );
}

// --- startup wiring ---------------------------------------------------

/// Loading happens before the first draw, so a restart repaints in the
/// saved theme rather than flashing the default one first.
#[test]
fn startup_restores_the_saved_theme() {
    let sandbox = Sandbox::new("startup-theme");
    let path = sandbox.file();
    prefs::save(
        &path,
        &Prefs {
            version: SCHEMA_VERSION,
            theme: Theme::Light.name().to_string(),
            focus_order: rotate_focus_order(),
            display_mode: prefs::DisplayMode::Fullscreen,
        },
    )
    .expect("save prefs");

    let app = app_with_prefs(&path);
    assert_eq!(app.theme, Theme::Light);
}

/// A console with no preferences file is a default console: the pre-theme
/// palette, an idle run, an empty transcript, the composer focused.
#[test]
fn a_console_with_no_preferences_file_uses_the_defaults() {
    let sandbox = Sandbox::new("no-file");
    let path = sandbox.file();
    assert!(!path.exists());

    let app = app_with_prefs(&path);
    assert_eq!(app.theme, Theme::default());
    assert_eq!(app.focus, rof::tui::app::Focus::Composer);
    assert_eq!(app.run_mode, rof::tui::app::RunMode::Idle);
    assert!(app.transcript.is_empty());
    assert!(app.run_goal.is_empty());
    assert!(app.input.is_empty());
    assert!(app.pending_steer.is_none());
    assert!(app.pending_goal.is_none());
    assert_eq!(app.next_control_id, 1);
    assert!(!path.exists(), "reading preferences created a file");
}

/// The allowlist is the whole of what survives: a live-ish console is
/// snapshotted, restarted, and comes back with nothing of the run.
#[test]
fn a_restart_restores_no_run_goal_transcript_queue_or_pending_control() {
    let sandbox = Sandbox::new("no-run-state");
    let path = sandbox.file();

    let mut live = App::new();
    live.begin_run("delete every test in the repo");
    live.transcript.push("steer 7: stop doing that".to_string());
    live.input.push_str("half-typed composer draft");
    live.on_event(&rof::obs::TraceEvent::ReviewVerdict {
        pass: true,
        feedback: "looks good".into(),
    });
    live.submit_pending_steer("stop");
    live.submit_pending_goal("and then ship it");
    live.deferred_config
        .push(rof::tui::app::DeferredConfig::Rounds(4));
    live.mask_input = true;
    live.set_replay_filter("needle");
    assert_eq!(live.run_mode, rof::tui::app::RunMode::Running);

    prefs::save(&path, &prefs::from_app(&live)).expect("save prefs");

    // None of that is in the file, and none of it comes back.
    let body = sandbox.read();
    for leak in [
        "delete every test",
        "half-typed",
        "stop doing that",
        "ship it",
        "looks good",
        "needle",
    ] {
        assert!(!body.contains(leak), "{leak} was persisted");
    }

    let restarted = app_with_prefs(&path);
    assert_eq!(restarted.run_mode, rof::tui::app::RunMode::Idle);
    assert!(restarted.run_goal.is_empty());
    assert!(restarted.transcript.is_empty());
    assert!(restarted.input.is_empty());
    assert!(restarted.pending_steer.is_none());
    assert!(restarted.pending_goal.is_none());
    assert!(restarted.deferred_config.is_empty());
    assert!(!restarted.mask_input);
    assert!(restarted.replay_filter.is_empty());
    assert_eq!(restarted.next_control_id, 1);
    assert_eq!(restarted.counters.model_calls, 0);
}

/// The store holds no secret and lives beside the credential store without
/// ever being it: one file in the directory, and nothing key-shaped in it.
#[test]
fn the_store_is_not_a_credential_store() {
    let sandbox = Sandbox::new("not-credentials");
    let path = sandbox.file();
    prefs::save(&path, &prefs::from_app(&App::new())).expect("save prefs");

    assert_eq!(
        sandbox.files(),
        vec![".rof".to_string()],
        "the store wrote beside the preferences file"
    );
    assert!(!path.parent().unwrap().join("credentials").exists());
    let body = sandbox.read();
    for forbidden in ["key", "token", "secret", "password", "credential"] {
        assert!(
            !body.to_lowercase().contains(forbidden),
            "{forbidden} is in the preferences file"
        );
    }
}

/// A theme change is the one allowlisted value a command writes, and the
/// snapshot taken after it is what the next console reads back.
#[test]
fn a_theme_change_is_what_the_next_console_reads() {
    let sandbox = Sandbox::new("theme-change");
    let path = sandbox.file();
    let mut app = app_with_prefs(&path);
    app.theme = Theme::Dark;
    prefs::save(&path, &prefs::from_app(&app)).expect("save prefs");

    let mut after = app_with_prefs(&path);
    assert_eq!(after.theme, Theme::Dark);
    after.theme = Theme::Light;
    prefs::save(&path, &prefs::from_app(&after)).expect("save again");
    assert_eq!(app_with_prefs(&path).theme, Theme::Light);
}

/// The store holds no secret, so it does not lock the file down the way
/// the credential store does: the preferences file is left at the process
/// umask, and only `~/.rof/credentials` is 0600. This asserts we did not
/// quietly start chmod-ing a file whose contents are four names.
#[test]
#[cfg(unix)]
fn the_preferences_file_is_not_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new("mode");
    let path = sandbox.file();
    prefs::save(&path, &prefs::from_app(&App::new())).expect("save prefs");

    // Compared against a plain sibling file, so the assertion is about what
    // the store DOES, not about the umask the test happens to run under.
    let probe = path.with_file_name("probe");
    std::fs::write(&probe, b"probe").expect("write probe");
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    assert_eq!(
        mode(&path),
        mode(&probe),
        "the store chmod-ed the preferences file: {:o} vs a plain file's {:o}",
        mode(&path),
        mode(&probe)
    );
}

/// The default location is `~/.rof/tui.json`; the env override exists so a
/// test can point the store at a temp directory without a new user-facing
/// knob. This asserts the shape, not a particular home.
#[test]
fn the_default_location_is_the_rof_tui_json_file() {
    let path = prefs::default_path();
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("tui.json"),
        "the default path is not tui.json: {path:?}"
    );
    let tail: Vec<String> = path
        .parent()
        .map(|parent| {
            parent
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .into_iter()
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        tail,
        vec![".rof".to_string()],
        "the default path is not under ~/.rof: {path:?}"
    );
}
