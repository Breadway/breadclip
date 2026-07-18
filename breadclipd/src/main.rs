mod content_kind;

use bread_utils::bread_client::BreadClient;
use breadclip_core::HistoryDb;
use std::{env, fs, path::PathBuf, process::Command, thread, time::Duration};

/// This app's id in bread's sibling-app namespace registry
/// (`bread_shared::apps::KNOWN_APPS`) — events are published as
/// `bread.clip.*`, commands are received on `bread.command.clip.*`.
const APP_ID: &str = "clip";

/// Argument used to invoke this same binary as the one-shot handler for
/// `wl-paste --watch`. Kept as a plain positional arg (no clap dependency
/// needed for a single internal flag).
const CAPTURE_FLAG: &str = "--capture-once";

/// MIME type convention (originating with KDE's Klipper) that password
/// managers such as KeePassXC and Bitwarden set on clipboard content they
/// own, signaling "don't persist this". Any offer advertising it is skipped
/// entirely.
const PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";

fn get_available_types() -> Vec<String> {
    Command::new("wl-paste")
        .args(["--list-types"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.lines().map(str::trim).map(String::from).collect())
        .unwrap_or_default()
}

fn get_clipboard_text() -> Option<String> {
    let output = Command::new("wl-paste")
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

fn get_clipboard_image() -> Option<Vec<u8>> {
    let output = Command::new("wl-paste")
        .args(["--type", "image/png"])
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    Some(output.stdout)
}

fn lock_file() -> PathBuf {
    env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("breadclipd.lock")
}

// Returns false if another instance is already running.
fn acquire_lock() -> bool {
    let path = lock_file();
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            let alive = fs::read_to_string(format!("/proc/{}/comm", pid))
                .map(|s| s.trim() == "breadclipd")
                .unwrap_or(false);
            if alive {
                eprintln!("breadclipd: already running (pid {})", pid);
                return false;
            }
        }
    }
    let _ = fs::write(&path, std::process::id().to_string());
    true
}

/// Invoked once per clipboard-change event (as the handler command for
/// `wl-paste --watch`). Reads whatever is on the clipboard right now, skips
/// it entirely if a password manager flagged it as sensitive, and otherwise
/// persists a text or image entry to the history DB.
fn capture_once() {
    let types = get_available_types();

    if types.iter().any(|t| t == PASSWORD_HINT_MIME) {
        // Password manager (KeePassXC, Bitwarden, etc.) marked this copy as
        // sensitive via the x-kde-passwordManagerHint convention — never
        // persist it.
        return;
    }

    let has_image = types.iter().any(|t| t == "image/png" || t == "image/jpeg");
    let has_text = types.iter().any(|t| t.starts_with("text/"));

    let db = match HistoryDb::open() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("breadclipd: failed to open database: {e}");
            return;
        }
    };

    if has_image && !has_text {
        if let Some(bytes) = get_clipboard_image() {
            match db.insert_image(&bytes) {
                Ok(()) => emit_copied("image", bytes.len()),
                Err(e) => eprintln!("breadclipd: insert image: {e}"),
            }
        }
    } else if has_text {
        if let Some(text) = get_clipboard_text() {
            match db.insert_text(&text) {
                Ok(()) => emit_copied(content_kind::detect(&text), text.len()),
                Err(e) => eprintln!("breadclipd: insert text: {e}"),
            }
        }
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

/// Reacts to `bread.command.clip.*` verbs. Only `clear` maps to real,
/// existing breadclip functionality today — `pin`/`select` would need a new
/// "pinned" concept that doesn't exist anywhere in the history DB schema,
/// which is a real product decision for breadclip itself (does it want
/// pinning at all, and what would the GTK UI for it look like?), not
/// something to fabricate as a side effect of wiring up the event bus. See
/// `breadclip/EVENTS.md` for the honest current status.
///
/// Emits `bread.clip.<verb>.done`/`.failed` per the confirmation convention
/// in bread's Documentation.md — a module that started this command via
/// `bread.wait`/`bread.wait_any` can await the real outcome instead of
/// assuming success the moment it publishes the command.
fn handle_command(event_name: &str) {
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
        other => {
            eprintln!("breadclipd: ignoring unrecognized command verb '{other}'");
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) == Some(CAPTURE_FLAG) {
        capture_once();
        return;
    }

    if !acquire_lock() {
        std::process::exit(1);
    }

    // Wait briefly for WAYLAND_DISPLAY — common when started early in the session
    let mut retries = 0;
    while env::var("WAYLAND_DISPLAY").is_err() && retries < 20 {
        thread::sleep(Duration::from_millis(500));
        retries += 1;
    }
    if env::var("WAYLAND_DISPLAY").is_err() {
        eprintln!("breadclipd: WAYLAND_DISPLAY not set after waiting, exiting");
        let _ = fs::remove_file(lock_file());
        std::process::exit(1);
    }

    // Fail fast (before we start watching) if the DB can't be opened.
    if let Err(e) = HistoryDb::open() {
        eprintln!("breadclipd: failed to open database: {e}");
        let _ = fs::remove_file(lock_file());
        std::process::exit(1);
    }

    let exe = match env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("breadclipd: could not resolve own executable path: {e}");
            let _ = fs::remove_file(lock_file());
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
        handle_command(&event.event);
    });

    // `wl-paste --watch <cmd>` runs <cmd> once per clipboard-change event
    // instead of polling — zero idle cost between changes, and no more
    // forking wl-paste 4-6 times a second. Each event re-invokes this same
    // binary with --capture-once to do a single read-and-store pass.
    loop {
        let status = Command::new("wl-paste")
            .args(["--watch"])
            .arg(&exe)
            .arg(CAPTURE_FLAG)
            .status();

        match status {
            Ok(s) => {
                eprintln!("breadclipd: wl-paste --watch exited ({s}), restarting in 2s");
            }
            Err(e) => {
                eprintln!("breadclipd: failed to spawn wl-paste --watch: {e}, retrying in 2s");
            }
        }
        thread::sleep(Duration::from_secs(2));
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
    fn handle_command_clear_empties_history_even_with_no_daemon_reachable() {
        // The point of this test: handle_command's actual effect (clearing
        // the DB) must not depend on breadd being reachable — `emit` inside
        // it is fire-and-forget and must never gate the real work. No test
        // daemon is running here, so if clearing the DB were accidentally
        // made contingent on the emit succeeding, this test would fail.
        with_isolated_history(|| {
            let db = HistoryDb::open().expect("open history db");
            db.insert_text("something to clear").unwrap();
            assert_eq!(db.list_entries(10).unwrap().len(), 1);

            handle_command("bread.command.clip.clear");

            let db = HistoryDb::open().expect("reopen history db");
            assert!(db.list_entries(10).unwrap().is_empty());
        });
    }

    #[test]
    fn handle_command_ignores_unrecognized_verb() {
        with_isolated_history(|| {
            let db = HistoryDb::open().expect("open history db");
            db.insert_text("should survive").unwrap();

            handle_command("bread.command.clip.pin");

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
            db.insert_text("should survive").unwrap();

            // Not a `bread.command.clip.*` event at all — must be a no-op.
            handle_command("bread.command.pad.clear");
            handle_command("bread.clip.copied");

            let db = HistoryDb::open().expect("reopen history db");
            assert_eq!(db.list_entries(10).unwrap().len(), 1);
        });
    }
}
