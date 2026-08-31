mod content_kind;
mod ignore_rules;

use bread_utils::bread_client::BreadClient;
use bread_utils::singleton::{try_acquire, Acquire};
use breadclip_core::{CaptureSource, HistoryDb};
use serde_json::Value;
use std::{env, io::Read, process::Command, thread, time::Duration};

/// This app's id in bread's sibling-app namespace registry
/// (`bread_shared::apps::KNOWN_APPS`) — events are published as
/// `bread.clip.*`, commands are received on `bread.command.clip.*`.
const APP_ID: &str = "clip";

/// Argument used to invoke this same binary as the one-shot handler for
/// `wl-paste --watch`. Kept as a plain positional arg (no clap dependency
/// needed for a single internal flag).
const CAPTURE_FLAG: &str = "--capture-once";

/// Second positional arg distinguishing a capture spawned by the primary-
/// selection watcher from one spawned by the regular-clipboard watcher.
const PRIMARY_FLAG: &str = "--primary";

/// MIME type convention (originating with KDE's Klipper) that password
/// managers such as KeePassXC and Bitwarden set on clipboard content they
/// own, signaling "don't persist this". Only reachable on the fallback
/// capture path — when invoked through `wl-paste --watch`, `CLIPBOARD_STATE`
/// carries the same information from the same event (see `capture_once`).
const PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// Build a `wl-paste` command with a hard deadline. Without the timeout, a
/// stalled selection offer (source app died mid-transfer, compositor never
/// completes the handoff) leaves `wl-paste` blocked forever reading a pipe
/// that never closes — and a forever-blocked fallback capture would eat a
/// file descriptor per event over a long session. `timeout -k` guarantees a
/// SIGKILL if the initial SIGTERM doesn't land. Only used by the manual
/// fallback path (`capture_via_wl_paste`); the `--watch` path reads the
/// content from stdin and so has nothing that can hang.
fn wl_paste_cmd(primary: bool) -> Command {
    let mut cmd = Command::new("timeout");
    cmd.args(["-k", "2", "5", "wl-paste"]);
    if primary {
        cmd.arg("--primary");
    }
    cmd
}

/// `wl-paste --watch` sets this in the spawned command's environment for
/// every clipboard event (see wl-paste(1)): `data` (read the content from
/// stdin), `nil` (empty clipboard), `clear` (explicitly cleared), or
/// `sensitive` (a password manager flagged the selection). Unset or
/// unrecognized means we were not invoked by `--watch` (e.g. a manual
/// `--capture-once` run) and we fall back to querying wl-paste directly.
const CLIPBOARD_STATE: &str = "CLIPBOARD_STATE";

/// Open the history DB with the retention caps from the user's config. The
/// `--capture-once` processes run per clipboard event, so they load the
/// config fresh rather than inheriting anything from the long-lived daemon.
fn open_db() -> Result<HistoryDb, breadclip_core::HistoryError> {
    HistoryDb::open_with(breadclip_core::config::load().retention)
}

fn get_available_types(primary: bool) -> Vec<String> {
    wl_paste_cmd(primary)
        .arg("--list-types")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().map(str::trim).map(String::from).collect())
        .unwrap_or_default()
}

fn get_clipboard_bytes(mime: &str, primary: bool) -> Option<Vec<u8>> {
    let output = wl_paste_cmd(primary).args(["--type", mime]).output().ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(output.stdout)
}

fn get_clipboard_text(primary: bool) -> Option<String> {
    let output = wl_paste_cmd(primary)
        .args(["--no-newline", "--type", "text/plain"])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Identify an image payload by its magic bytes. `wl-paste --watch` hands us
/// the content on stdin without saying which offered type it picked, so the
/// bytes are the only reliable signal — and they're exact, because watch
/// mode never appends a trailing newline.
fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    None
}

fn source(primary: bool) -> CaptureSource {
    if primary {
        CaptureSource::Primary
    } else {
        CaptureSource::Clipboard
    }
}

fn store_text(text: &str, primary: bool) {
    let db = match open_db() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("breadclipd: failed to open database: {e}");
            return;
        }
    };
    match db.insert_text(text, source(primary)) {
        Ok(()) => emit_copied(content_kind::detect(text), text.len()),
        Err(e) => eprintln!("breadclipd: insert text: {e}"),
    }
}

fn store_image(bytes: Vec<u8>, mime: &str, primary: bool) {
    let db = match open_db() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("breadclipd: failed to open database: {e}");
            return;
        }
    };
    match db.insert_image(&bytes, mime, source(primary)) {
        Ok(()) => emit_copied("image", bytes.len()),
        Err(e) => eprintln!("breadclipd: insert image: {e}"),
    }
}

/// Capture path for `wl-paste --watch` invocations: the clipboard content
/// arrives on our stdin together with its `CLIPBOARD_STATE` in one event, so
/// the sensitive check and the data read can't race each other (two separate
/// `wl-paste` calls could straddle a clipboard change mid-capture).
fn capture_from_stdin(primary: bool) {
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() || buf.is_empty() {
        return;
    }
    if let Some(mime) = sniff_image_mime(&buf) {
        store_image(buf, mime, primary);
    } else if let Ok(text) = String::from_utf8(buf) {
        if !text.trim().is_empty() && !ignore_rules::is_sensitive(&text) {
            store_text(&text, primary);
        }
    }
}

/// Fallback capture path for manual `--capture-once` invocations (no
/// `CLIPBOARD_STATE` was set by wl-paste): ask wl-paste directly. Same
/// behavior as the stdin path, including requesting whichever image type is
/// actually offered rather than assuming PNG.
fn capture_via_wl_paste(primary: bool) {
    let types = get_available_types(primary);

    if types.iter().any(|t| t == PASSWORD_HINT_MIME) {
        // Password manager (KeePassXC, Bitwarden, etc.) marked this copy as
        // sensitive via the x-kde-passwordManagerHint convention — never
        // persist it.
        return;
    }

    let has_image = types.iter().any(|t| t == "image/png" || t == "image/jpeg");
    let has_text = types.iter().any(|t| t.starts_with("text/"));

    if has_image && !has_text {
        let mime = if types.iter().any(|t| t == "image/png") {
            "image/png"
        } else {
            "image/jpeg"
        };
        if let Some(bytes) = get_clipboard_bytes(mime, primary) {
            store_image(bytes, mime, primary);
        }
    } else if has_text {
        if let Some(text) = get_clipboard_text(primary) {
            if !ignore_rules::is_sensitive(&text) {
                store_text(&text, primary);
            }
        }
    }
}

/// Invoked once per clipboard-change event (as the handler command for
/// `wl-paste --watch`). Stores a text or image entry to the history DB,
/// unless the selection was flagged as sensitive — in which case nothing is
/// ever persisted.
fn capture_once(primary: bool) {
    match env::var(CLIPBOARD_STATE).as_deref() {
        // Password manager (KeePassXC, Bitwarden, ...) flagged this selection
        // as sensitive via `wl-copy --sensitive` — never persist it.
        Ok("sensitive") => {}
        // Empty clipboard (nil) or an explicit clear — nothing to store.
        Ok("nil") | Ok("clear") => {}
        Ok("data") => capture_from_stdin(primary),
        // Unset or unrecognized: not invoked by `--watch`. Fall back to the
        // direct wl-paste path rather than blocking on whatever stdin is.
        _ => capture_via_wl_paste(primary),
    }
}

/// Publishes `bread.clip.copied` into the bread event fabric. Fire-and-forget
/// and non-fatal by design (`BreadClient::emit` never blocks or errors this
/// caller) — breadd being absent or not installed must never affect
/// breadclip's own clipboard-history functionality, only mean this one
/// notification doesn't go anywhere.
fn emit_copied(kind: &str, len: usize) {
    BreadClient::connect(APP_ID).emit(
        "bread.clip.copied",
        serde_json::json!({ "kind": kind, "len": len }),
    );
}

/// Reacts to `bread.command.clip.*` verbs. Only `clear` and `pin` map to
/// real, existing breadclip functionality today — `select` would need a
/// "remote-activate a row" concept the popup doesn't expose over the bus,
/// which is a real product decision for breadclip itself, not something to
/// fabricate as a side effect of wiring up the event bus. See
/// `breadclip/EVENTS.md` for the honest current status.
///
/// Emits `bread.clip.<verb>.done`/`bread.clip.pinned`/`.failed` per the
/// confirmation convention in bread's Documentation.md — a module that
/// started this command via `bread.wait`/`bread.wait_any` can await the real
/// outcome instead of assuming success the moment it publishes the command.
fn handle_command(event_name: &str, data: &Value) {
    let Some(verb) = event_name.strip_prefix("bread.command.clip.") else {
        return;
    };
    match verb {
        "clear" => match HistoryDb::open().and_then(|db| db.clear_all()) {
            Ok(()) => {
                eprintln!("breadclipd: cleared history via bread.command.clip.clear");
                BreadClient::connect(APP_ID).emit("bread.clip.clear.done", serde_json::json!({}));
            }
            Err(e) => {
                eprintln!("breadclipd: bread.command.clip.clear failed: {e}");
                BreadClient::connect(APP_ID).emit(
                    "bread.clip.clear.failed",
                    serde_json::json!({ "error": e.to_string() }),
                );
            }
        },
        "pin" => match data.get("id").and_then(Value::as_i64) {
            Some(id) => {
                let pin = data.get("pin").and_then(Value::as_bool).unwrap_or(true);
                match HistoryDb::open().and_then(|db| db.set_pinned(id, pin)) {
                    Ok(()) => {
                        eprintln!(
                            "breadclipd: {} entry {id} via bread.command.clip.pin",
                            if pin { "pinned" } else { "unpinned" }
                        );
                        BreadClient::connect(APP_ID).emit(
                            "bread.clip.pinned",
                            serde_json::json!({ "id": id, "pinned": pin }),
                        );
                    }
                    Err(e) => {
                        eprintln!("breadclipd: bread.command.clip.pin failed: {e}");
                        BreadClient::connect(APP_ID).emit(
                            "bread.clip.pin.failed",
                            serde_json::json!({ "error": e.to_string() }),
                        );
                    }
                }
            }
            None => {
                eprintln!("breadclipd: bread.command.clip.pin missing 'id'");
                BreadClient::connect(APP_ID).emit(
                    "bread.clip.pin.failed",
                    serde_json::json!({ "error": "missing 'id'" }),
                );
            }
        },
        other => {
            eprintln!("breadclipd: ignoring unrecognized command verb '{other}'");
        }
    }
}

/// Backoff for the `wl-paste --watch` restart loops: 2s → 4s → 8s → 16s →
/// 32s, capped at 30s. A dead compositor keeps the daemon alive without
/// hammering either wl-paste or the journal.
fn restart_delay(consecutive_failures: u32) -> Duration {
    Duration::from_secs((2u64 << consecutive_failures.saturating_sub(1).min(4)).min(30))
}

/// Log a watch-loop restart, but only on state changes — the first failure
/// announces it, then every 8th attempt. Otherwise a persistent failure
/// would spam the journal every two seconds forever.
fn log_restart(what: &str, detail: &str, attempt: u32) {
    if attempt == 1 || attempt.is_multiple_of(8) {
        eprintln!("breadclipd: {what} ({detail}), restarting (attempt {attempt})");
    }
}

/// Runs one `wl-paste --watch` watcher for the daemon's whole lifetime,
/// restarting it with capped backoff whenever it exits (e.g. the compositor
/// connection dropped). `primary` selects the middle-click primary selection
/// watcher (`--watch --primary`) instead of the regular clipboard watcher.
fn watch_loop(exe: std::path::PathBuf, primary: bool) {
    let label = if primary {
        "wl-paste --watch (primary)"
    } else {
        "wl-paste --watch"
    };
    let mut consecutive_failures: u32 = 0;
    loop {
        // `wl-paste --watch <cmd>` runs <cmd> once per selection-change
        // event instead of polling — zero idle cost between changes, and no
        // forking wl-paste repeatedly. Each event re-invokes this same
        // binary with --capture-once to do a single read-and-store pass.
        let mut cmd = Command::new("wl-paste");
        cmd.arg("--watch");
        if primary {
            cmd.arg("--primary");
        }
        cmd.arg(&exe).arg(CAPTURE_FLAG);
        if primary {
            cmd.arg(PRIMARY_FLAG);
        }
        let status = cmd.status();

        match status {
            Ok(s) => {
                consecutive_failures += 1;
                log_restart(label, &format!("{s}"), consecutive_failures);
            }
            Err(e) => {
                consecutive_failures += 1;
                log_restart(label, &e.to_string(), consecutive_failures);
            }
        }
        thread::sleep(restart_delay(consecutive_failures));
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) == Some(CAPTURE_FLAG) {
        capture_once(args.iter().any(|a| a == PRIMARY_FLAG));
        return;
    }

    // flock(2)-based singleton (bread_utils::singleton): lock ownership is
    // kernel-atomic and released automatically the instant this process
    // dies, so there's no stale-pid-file case to reason about.
    let _singleton = match try_acquire("breadclipd") {
        Ok(Acquire::Acquired(guard)) => Some(guard),
        Ok(Acquire::HeldByOther(pid)) => {
            eprintln!("breadclipd: already running (pid {:?})", pid);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("breadclipd: single-instance lock unavailable ({e}); continuing without it");
            None
        }
    };

    // Wait briefly for WAYLAND_DISPLAY — common when started early in the session
    let mut retries = 0;
    while env::var("WAYLAND_DISPLAY").is_err() && retries < 20 {
        thread::sleep(Duration::from_millis(500));
        retries += 1;
    }
    if env::var("WAYLAND_DISPLAY").is_err() {
        eprintln!("breadclipd: WAYLAND_DISPLAY not set after waiting, exiting");
        std::process::exit(1);
    }

    let cfg = breadclip_core::config::load();

    // Fail fast (before we start watching) if the DB can't be opened.
    if let Err(e) = HistoryDb::open_with(cfg.retention) {
        eprintln!("breadclipd: failed to open database: {e}");
        std::process::exit(1);
    }

    let exe = match env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("breadclipd: could not resolve own executable path: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("breadclipd: started (pid {})", std::process::id());

    // Long-lived, so this uses BreadClient::subscribe (a persistent
    // background thread with its own reconnect/backoff loop) rather than a
    // one-shot connection — the same client type capture_once() uses for
    // `emit`, just the other half of it. breadd being absent or restarting
    // is transparent here too: the subscription just quietly stops
    // delivering commands until it reconnects.
    let command_client = BreadClient::connect(APP_ID);
    let _commands = command_client.subscribe("bread.command.clip.**", |event| {
        handle_command(&event.event, &event.data);
    });

    // Regular-clipboard watcher, always on.
    let clipboard_exe = exe.clone();
    thread::spawn(move || watch_loop(clipboard_exe, false));
    // Primary-selection watcher (middle-click), opt-in via config.
    if cfg.capture_primary {
        eprintln!("breadclipd: watching primary selection");
        thread::spawn(move || watch_loop(exe, true));
    }

    // The watcher threads own the daemon's lifetime; this thread just keeps
    // the process (and the singleton guard above) alive.
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Isolates HistoryDb::open() to a fresh temp dir per test. `cargo test`
    // runs tests in parallel threads within one process by default, and
    // `XDG_DATA_HOME` is process-global — three tests in this module all
    // set it, so without this lock they race each other (confirmed: an
    // earlier version of this file had that exact bug, caught by a flaky
    // `len() == 2` instead of `1` failure).
    fn env_test_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn with_isolated_history<F: FnOnce()>(f: F) {
        let _guard = env_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("XDG_DATA_HOME", dir.path());
        f();
    }

    #[test]
    fn sniff_image_mime_detects_png_and_jpeg() {
        assert_eq!(sniff_image_mime(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(b"plain text"), None);
    }

    #[test]
    fn restart_delay_backs_off_and_caps() {
        assert_eq!(restart_delay(1), Duration::from_secs(2));
        assert_eq!(restart_delay(2), Duration::from_secs(4));
        assert_eq!(restart_delay(3), Duration::from_secs(8));
        assert_eq!(restart_delay(4), Duration::from_secs(16));
        assert_eq!(restart_delay(100), Duration::from_secs(30));
    }

    #[test]
    fn handle_command_clear_empties_history_even_with_no_daemon_reachable() {
        // The point of this test: handle_command's actual effect (clearing
        // the DB) must not depend on breadd being reachable — `emit` inside
        // it is fire-and-forget and must never gate the real work. No test
        // daemon is running here, so if clearing the DB were accidentally
        // made contingent on the emit succeeding, this test would fail.
        with_isolated_history(|| {
            let db = HistoryDb::open().expect("open history db");
            db.insert_text("something to clear", CaptureSource::Clipboard).unwrap();
            assert_eq!(db.list_entries(10).unwrap().len(), 1);

            handle_command("bread.command.clip.clear", &serde_json::json!({}));

            let db = HistoryDb::open().expect("reopen history db");
            assert!(db.list_entries(10).unwrap().is_empty());
        });
    }

    #[test]
    fn handle_command_pin_toggles_pinned_state() {
        with_isolated_history(|| {
            let db = HistoryDb::open().expect("open history db");
            db.insert_text("to pin", CaptureSource::Clipboard).unwrap();
            let id = db.list_entries(10).unwrap()[0].id;
            assert!(!db.list_entries(10).unwrap()[0].pinned);

            handle_command(
                "bread.command.clip.pin",
                &serde_json::json!({ "id": id, "pin": true }),
            );
            assert!(db.list_entries(10).unwrap()[0].pinned);

            handle_command(
                "bread.command.clip.pin",
                &serde_json::json!({ "id": id, "pin": false }),
            );
            assert!(!db.list_entries(10).unwrap()[0].pinned);
        });
    }

    #[test]
    fn handle_command_pin_without_id_is_a_no_op() {
        with_isolated_history(|| {
            let db = HistoryDb::open().expect("open history db");
            db.insert_text("untouched", CaptureSource::Clipboard).unwrap();

            handle_command("bread.command.clip.pin", &serde_json::json!({}));

            assert!(!db.list_entries(10).unwrap()[0].pinned);
        });
    }

    #[test]
    fn handle_command_ignores_unrecognized_verb() {
        with_isolated_history(|| {
            let db = HistoryDb::open().expect("open history db");
            db.insert_text("should survive", CaptureSource::Clipboard).unwrap();

            handle_command("bread.command.clip.select", &serde_json::json!({}));

            let db = HistoryDb::open().expect("reopen history db");
            assert_eq!(
                db.list_entries(10).unwrap().len(),
                1,
                "an unrecognized verb must not touch history"
            );
        });
    }

    #[test]
    fn handle_command_ignores_events_outside_its_own_command_namespace() {
        with_isolated_history(|| {
            let db = HistoryDb::open().expect("open history db");
            db.insert_text("should survive", CaptureSource::Clipboard).unwrap();

            // Not a `bread.command.clip.*` event at all — must be a no-op.
            handle_command("bread.command.pad.clear", &serde_json::json!({}));
            handle_command("bread.clip.copied", &serde_json::json!({}));

            let db = HistoryDb::open().expect("reopen history db");
            assert_eq!(db.list_entries(10).unwrap().len(), 1);
        });
    }
}
