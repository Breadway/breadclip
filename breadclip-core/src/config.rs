//! breadclip's user configuration.
//!
//! Read from `$XDG_CONFIG_HOME/breadclip/config.toml` (or
//! `~/.config/breadclip/config.toml`). Every key has a sensible default, so
//! the file is entirely optional. A file that *exists* but fails to parse is
//! backed up to `config.toml.bak` (once) and defaults are used, matching
//! bread-utils' non-destructive TOML discipline (see
//! `bread_utils::tomlcfg::load_doc`).
//!
//! ```toml
//! [retention]
//! text = 200    # max non-pinned text entries (0 = keep none)
//! images = 50   # max non-pinned image entries (0 = keep none)
//!
//! [panel]
//! width = 520   # popup panel width, px
//!
//! [capture]
//! primary = false  # also watch the middle-click primary selection
//! ```

use crate::Retention;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Item};

/// Resolved configuration. `Config::default()` matches the built-in
/// behavior before config files existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub retention: Retention,
    pub panel_width: i32,
    pub capture_primary: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            retention: Retention::default(),
            panel_width: 520,
            capture_primary: false,
        }
    }
}

/// Clamp bounds. Retention can legitimately be `0` ("keep no unpinned
/// entries of this kind") but never huge; the panel width is bounded to
/// something that still fits on a screen.
const MAX_RETENTION: i64 = 10_000;
const MIN_PANEL_WIDTH: i64 = 300;
const MAX_PANEL_WIDTH: i64 = 2000;

pub fn config_path() -> PathBuf {
    bread_utils::xdg::config_dir("breadclip").join("config.toml")
}

/// Load configuration, falling back to defaults for anything missing,
/// unparseable, or out of range.
pub fn load() -> Config {
    let doc = bread_utils::tomlcfg::load_doc("breadclip", &config_path());
    Config {
        retention: Retention {
            text: int(&doc, "retention", "text", Retention::default().text as i64)
                .clamp(0, MAX_RETENTION) as usize,
            images: int(&doc, "retention", "images", Retention::default().images as i64)
                .clamp(0, MAX_RETENTION) as usize,
        },
        panel_width: int(&doc, "panel", "width", 520)
            .clamp(MIN_PANEL_WIDTH, MAX_PANEL_WIDTH) as i32,
        capture_primary: boolean(&doc, "capture", "primary", false),
    }
}

fn int(doc: &DocumentMut, section: &str, key: &str, default: i64) -> i64 {
    doc.get(section)
        .and_then(Item::as_table)
        .and_then(|t| t.get(key))
        .and_then(Item::as_integer)
        .unwrap_or(default)
}

fn boolean(doc: &DocumentMut, section: &str, key: &str, default: bool) -> bool {
    doc.get(section)
        .and_then(Item::as_table)
        .and_then(|t| t.get(key))
        .and_then(Item::as_bool)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_config_dir<F: FnOnce() -> T, T>(f: F) -> T {
        let _guard = crate::env_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let r = f();
        let _ = std::fs::remove_dir_all(dir.path());
        r
    }

    fn write_config(body: &str) {
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
    }

    #[test]
    fn missing_config_yields_defaults() {
        with_config_dir(|| {
            assert_eq!(load(), Config::default());
        });
    }

    #[test]
    fn parses_known_keys() {
        with_config_dir(|| {
            write_config(
                r#"
[retention]
text = 10
images = 0

[panel]
width = 700

[capture]
primary = true
"#,
            );
            let cfg = load();
            assert_eq!(cfg.retention.text, 10);
            assert_eq!(cfg.retention.images, 0, "0 retention is legal");
            assert_eq!(cfg.panel_width, 700);
            assert!(cfg.capture_primary);
        });
    }

    #[test]
    fn out_of_range_values_are_clamped_not_trusted() {
        with_config_dir(|| {
            write_config(
                r#"
[retention]
text = -5
images = 99999999

[panel]
width = 50
"#,
            );
            let cfg = load();
            assert_eq!(cfg.retention.text, 0);
            assert_eq!(cfg.retention.images, MAX_RETENTION as usize);
            assert_eq!(cfg.panel_width, MIN_PANEL_WIDTH as i32);
        });
    }

    #[test]
    fn partial_config_falls_back_per_key() {
        with_config_dir(|| {
            write_config(
                r#"
[retention]
text = 7
"#,
            );
            let cfg = load();
            assert_eq!(cfg.retention.text, 7);
            assert_eq!(cfg.retention.images, Retention::default().images);
            assert_eq!(cfg.panel_width, Config::default().panel_width);
            assert!(!cfg.capture_primary);
        });
    }
}
