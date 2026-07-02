use breadclip_core::{sha256_hex, HistoryDb};
use std::{env, fs, path::PathBuf, process::Command, thread, time::Duration};

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

fn main() {
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

    let db = match HistoryDb::open() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("breadclipd: failed to open database: {e}");
            let _ = fs::remove_file(lock_file());
            std::process::exit(1);
        }
    };

    eprintln!("breadclipd: started (pid {})", std::process::id());

    let mut last_text_hash: Option<String> = None;
    let mut last_image_hash: Option<String> = None;

    loop {
        let types = get_available_types();
        let has_image = types.iter().any(|t| t == "image/png" || t == "image/jpeg");
        let has_text = types.iter().any(|t| t.starts_with("text/"));

        if has_image && !has_text {
            if let Some(bytes) = get_clipboard_image() {
                let hash = sha256_hex(&bytes);
                if Some(&hash) != last_image_hash.as_ref() {
                    last_image_hash = Some(hash);
                    last_text_hash = None;
                    if let Err(e) = db.insert_image(&bytes) {
                        eprintln!("breadclipd: insert image: {e}");
                    }
                }
            }
        } else if has_text {
            if let Some(text) = get_clipboard_text() {
                let hash = sha256_hex(text.as_bytes());
                if Some(&hash) != last_text_hash.as_ref() {
                    last_text_hash = Some(hash);
                    if let Err(e) = db.insert_text(&text) {
                        eprintln!("breadclipd: insert text: {e}");
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(500));
    }
}
