//! `--screenshot` CLI mode: render breadclip's clipboard-history panel,
//! capture it via `bread-screenshots`, then exit, driven by
//! `bread-ecosystem`'s `bread-capture` orchestrator, or run standalone for
//! one-off captures.
//!
//! breadclip has one view worth capturing: the history panel itself. Like
//! breadbox, it's a `halign`/`valign`-positioned panel over a full-screen
//! transparent overlay rather than its own layer surface, so a full
//! known-size canvas capture is the simplest reliable option. In screenshot
//! mode there's never a focused window to anchor the panel next to (see
//! `bread_utils::hypr::active_window` in `main.rs`, which the isolated
//! capture environment always reports `None` for, deliberately, see
//! `bread-capture`'s isolation module), so the panel always falls back to
//! centered, which is exactly what we want for a consistent screenshot.

use bread_utils::screenshot_cli::{validate_pair, DEFAULT_HEIGHT, DEFAULT_WIDTH, SETTLE_DELAY};
use clap::Parser;
use gtk4::prelude::*;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "breadclip")]
pub struct Cli {
    /// Render the named view, capture it, then exit instead of running
    /// normally. Known views: "history".
    #[arg(long)]
    pub screenshot: Option<String>,

    /// PNG path to write the capture to. Required together with --screenshot.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Capture canvas width: matches the isolated compositor's output width
    /// (`bread-capture --isolate-width`).
    #[arg(long, default_value_t = DEFAULT_WIDTH)]
    pub width: u32,

    /// Capture canvas height (see `width`).
    #[arg(long, default_value_t = DEFAULT_HEIGHT)]
    pub height: u32,
}

#[derive(Clone)]
pub struct ScreenshotRequest {
    pub view: String,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
}

impl Cli {
    /// `None` for a normal run. Exits the process with an error if the
    /// `--screenshot` / `--output` pair is incomplete, before any GTK setup
    /// happens.
    pub fn screenshot_request(&self) -> Option<ScreenshotRequest> {
        if let Err(e) = validate_pair(self.screenshot.as_deref(), self.output.as_deref()) {
            eprintln!("breadclip: {e}");
            std::process::exit(1);
        }
        Some(ScreenshotRequest {
            view: self.screenshot.clone()?,
            output: self.output.clone()?,
            width: self.width,
            height: self.height,
        })
    }
}

/// Wire up the given view's screenshot sequence against an already-built,
/// not-yet-presented window. Every path here ends by exiting the process;
/// it never returns control to the normal history-panel UI.
pub fn dispatch(window: &gtk4::ApplicationWindow, req: ScreenshotRequest) {
    match req.view.as_str() {
        "history" => {
            let output = req.output;
            let (width, height) = (req.width as i32, req.height as i32);
            window.connect_map(move |_| {
                let output = output.clone();
                gtk4::glib::timeout_add_local_once(SETTLE_DELAY, move || {
                    finish(bread_screenshots::capture_region(
                        0, 0, width, height, &output,
                    ));
                });
            });
        }
        other => {
            eprintln!("breadclip: unknown screenshot view '{other}' (known: history)");
            std::process::exit(1);
        }
    }
}

fn finish(result: anyhow::Result<()>) {
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("breadclip: screenshot capture failed: {e}");
            std::process::exit(1);
        }
    }
}
