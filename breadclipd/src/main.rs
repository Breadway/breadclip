use breadclip_core::HistoryDb;
use std::{env, fs, path::PathBuf, process::Command, thread, time::Duration};

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
            if let Err(e) = db.insert_image(&bytes) {
                eprintln!("breadclipd: insert image: {e}");
            }
        }
    } else if has_text {
        if let Some(text) = get_clipboard_text() {
            if let Err(e) = db.insert_text(&text) {
                eprintln!("breadclipd: insert text: {e}");
            }
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
