mod css;
mod position;

use breadclip_core::{ClipEntry, HistoryDb};
use bread_theme::{load_palette};
use gtk4::{
    glib,
    pango::EllipsizeMode,
    prelude::*,
    Application, ApplicationWindow, Box as GBox, Button, ContentFit, EventControllerKey, Label,
    ListBox, Orientation, Picture, PolicyType, ScrolledWindow, SearchEntry, SelectionMode,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::{
    cell::Cell,
    env,
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    rc::Rc,
};

const PANEL_WIDTH: i32 = 520;
const MAX_ENTRIES: usize = 200;
const PANEL_GAP: i32 = 8; // gap between focused window bottom and panel top
// Worst-case panel height (search + chips + full-height list), used to keep
// the panel fully on-screen before its natural size is known.
const PANEL_HEIGHT_ESTIMATE: i32 = 580;

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Text,
    Images,
}

// ---- Helpers ----------------------------------------------------------------

fn format_timestamp(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = now.saturating_sub(ts);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

fn get_row_entry(row: &gtk4::ListBoxRow) -> Option<ClipEntry> {
    unsafe { row.data::<ClipEntry>("entry").map(|p| p.as_ref().clone()) }
}

fn entry_matches(entry: &ClipEntry, query: &str, filter: Filter) -> bool {
    let is_image = entry.mime_type.starts_with("image/");
    let type_ok = match filter {
        Filter::All => true,
        Filter::Text => !is_image,
        Filter::Images => is_image,
    };
    if !type_ok {
        return false;
    }
    if query.is_empty() {
        return true;
    }
    if is_image {
        // Images are unsearchable by text; only show in Images filter
        return filter == Filter::Images;
    }
    entry
        .content
        .as_deref()
        .map(|t| t.to_lowercase().contains(&query.to_lowercase()))
        .unwrap_or(false)
}

fn refresh_list(list: &ListBox, query: &str, filter: Filter) {
    let mut i = 0i32;
    while let Some(row) = list.row_at_index(i) {
        let vis = get_row_entry(&row)
            .map(|e| entry_matches(&e, query, filter))
            .unwrap_or(false);
        row.set_visible(vis);
        i += 1;
    }
    let first = (0i32..).find_map(|j| list.row_at_index(j).filter(|r| r.is_visible()));
    list.select_row(first.as_ref());
}

fn build_row(entry: &ClipEntry) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.add_css_class("clip-row");

    let hbox = GBox::new(Orientation::Horizontal, 8);
    hbox.set_valign(gtk4::Align::Center);

    if entry.mime_type.starts_with("image/") {
        // Thumbnail
        let thumb = if let Some(ref path) = entry.image_path {
            let pic = Picture::new();
            pic.set_filename(Some(path.as_str()));
            pic.set_can_shrink(true);
            pic.set_content_fit(ContentFit::Contain);
            pic.add_css_class("clip-thumbnail");
            pic
        } else {
            let pic = Picture::new();
            pic.add_css_class("clip-thumbnail");
            pic
        };
        hbox.append(&thumb);

        let type_lbl = Label::new(Some("image"));
        type_lbl.add_css_class("clip-meta");
        type_lbl.set_hexpand(true);
        type_lbl.set_xalign(0.0);
        hbox.append(&type_lbl);
    } else {
        // Text preview — first line, truncated
        let preview = entry
            .content
            .as_deref()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();

        let text_lbl = Label::new(Some(&preview));
        text_lbl.add_css_class("clip-text-preview");
        text_lbl.set_hexpand(true);
        text_lbl.set_xalign(0.0);
        text_lbl.set_ellipsize(EllipsizeMode::End);
        text_lbl.set_single_line_mode(true);
        hbox.append(&text_lbl);
    }

    let ts_lbl = Label::new(Some(&format_timestamp(entry.timestamp)));
    ts_lbl.add_css_class("clip-meta");
    ts_lbl.set_xalign(1.0);
    hbox.append(&ts_lbl);

    row.set_child(Some(&hbox));
    unsafe { row.set_data("entry", entry.clone()) };
    row
}

fn do_copy(entry: &ClipEntry) {
    match entry.mime_type.as_str() {
        "image/png" => {
            if let Some(ref path) = entry.image_path {
                if let Ok(file) = fs::File::open(path) {
                    let _ = Command::new("wl-copy")
                        .args(["--type", "image/png"])
                        .stdin(Stdio::from(file))
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn();
                }
            }
        }
        _ => {
            if let Some(ref content) = entry.content {
                if let Ok(mut child) = Command::new("wl-copy")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(content.as_bytes());
                    }
                }
            }
        }
    }
}

// ---- PID file toggle (single-instance, matches breadbox pattern) -------------

fn pid_file() -> PathBuf {
    env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("breadclip.pid")
}

fn toggle_or_continue() -> bool {
    let pf = pid_file();
    if let Ok(content) = fs::read_to_string(&pf) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            let alive = fs::read_to_string(format!("/proc/{}/comm", pid))
                .map(|s| s.trim() == "breadclip")
                .unwrap_or(false);
            if alive {
                let _ = Command::new("kill").arg(pid.to_string()).status();
                return false;
            }
        }
    }
    let _ = fs::write(&pf, std::process::id().to_string());
    true
}

fn cleanup_pid() {
    let _ = fs::remove_file(pid_file());
}

// ---- UI ---------------------------------------------------------------------

fn run_ui(entries: Vec<ClipEntry>) {
    let app = Application::builder()
        .application_id("com.breadway.breadclip")
        .build();

    app.connect_activate(move |app| {
        bread_theme::gtk::apply_shared();
        bread_theme::gtk::apply_app_css(|| css::build_css(&load_palette()));

        // Full-screen transparent overlay; panel widget is positioned inside it.
        let window = ApplicationWindow::builder().application(app).build();
        window.init_layer_shell();
        window.set_namespace(Some("breadclip"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(0);

        // ---- Position panel relative to the active window ----
        // If a non-fullscreen window is focused, anchor the panel just below it.
        // Otherwise, centre the panel on screen.
        let active_win = position::get_active_window();
        let monitor = position::get_focused_monitor();

        let panel = GBox::new(Orientation::Vertical, 0);
        panel.add_css_class("clip-panel");
        panel.set_size_request(PANEL_WIDTH, -1);

        if let Some(ref win) = active_win {
            let (mon_x, mon_y, mon_w, mon_h) = monitor
                .as_ref()
                .map(|m| (m.x, m.y, m.width, m.height))
                .unwrap_or((0, 0, 1920, 1080));

            // Clamp horizontally so the panel never runs off the left/right
            // edge of the focused monitor.
            let clamped_left = win
                .x
                .min(mon_x + mon_w - PANEL_WIDTH - PANEL_GAP)
                .max(mon_x + PANEL_GAP);

            // Prefer anchoring below the window, but flip above it when there
            // isn't enough room underneath (e.g. a maximized/tiled window
            // with a text box near the bottom of the screen) — otherwise the
            // panel gets pushed off-screen and never becomes visible.
            let space_below = (mon_y + mon_h) - (win.y + win.height + PANEL_GAP);
            let space_above = win.y - mon_y - PANEL_GAP;
            let top = if space_below >= PANEL_HEIGHT_ESTIMATE || space_below >= space_above {
                win.y + win.height + PANEL_GAP
            } else {
                win.y - PANEL_GAP - PANEL_HEIGHT_ESTIMATE
            };
            let clamped_top = top
                .min(mon_y + mon_h - PANEL_HEIGHT_ESTIMATE - PANEL_GAP)
                .max(mon_y + PANEL_GAP);

            panel.set_halign(gtk4::Align::Start);
            panel.set_valign(gtk4::Align::Start);
            panel.set_margin_top(clamped_top);
            panel.set_margin_start(clamped_left);
        } else {
            panel.set_halign(gtk4::Align::Center);
            panel.set_valign(gtk4::Align::Center);
        }

        // ---- Search entry ----
        let search = SearchEntry::new();
        search.set_placeholder_text(Some("Search clipboard…"));
        panel.append(&search);

        // ---- Filter chips ----
        let chips_box = GBox::new(Orientation::Horizontal, 4);
        chips_box.add_css_class("chips-row");

        let chip_labels = ["All", "Text", "Images"];
        let chip_filters = [Filter::All, Filter::Text, Filter::Images];
        let chip_buttons: Vec<Button> = chip_labels
            .iter()
            .map(|lbl| {
                let b = Button::with_label(lbl);
                b.add_css_class("chip");
                b
            })
            .collect();
        chip_buttons[0].add_css_class("active");
        for btn in &chip_buttons {
            chips_box.append(btn);
        }
        panel.append(&chips_box);

        // ---- Scrolled list ----
        let scroll = ScrolledWindow::new();
        scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
        scroll.set_max_content_height(480);
        scroll.set_propagate_natural_height(true);

        let list = ListBox::new();
        list.set_selection_mode(SelectionMode::Browse);

        for entry in &entries {
            list.append(&build_row(entry));
        }
        if let Some(first) = list.row_at_index(0) {
            list.select_row(Some(&first));
        }

        scroll.set_child(Some(&list));
        panel.append(&scroll);
        window.set_child(Some(&panel));

        // ---- Shared state ----
        let query_rc: Rc<std::cell::RefCell<String>> = Rc::new(std::cell::RefCell::new(String::new()));
        let filter_rc: Rc<Cell<Filter>> = Rc::new(Cell::new(Filter::All));

        let close_all: Rc<dyn Fn()> = Rc::new({
            let w = window.clone();
            move || {
                cleanup_pid();
                w.close();
            }
        });

        // ---- Filter chip handlers ----
        let buttons_rc = Rc::new(chip_buttons);
        for (i, filter_val) in chip_filters.iter().enumerate() {
            let filter_r = Rc::clone(&filter_rc);
            let buttons_r = Rc::clone(&buttons_rc);
            let buttons_for_closure = Rc::clone(&buttons_rc);
            let list_r = list.clone();
            let query_r = Rc::clone(&query_rc);
            let fv = *filter_val;
            // Borrow buttons_r only for the index, move the separate clone into the closure
            buttons_r[i].connect_clicked(move |_| {
                filter_r.set(fv);
                for (j, b) in buttons_for_closure.iter().enumerate() {
                    if j == i {
                        b.add_css_class("active");
                    } else {
                        b.remove_css_class("active");
                    }
                }
                refresh_list(&list_r, &query_r.borrow(), fv);
            });
        }

        // ---- Search handler ----
        {
            let list_f = list.clone();
            let filter_f = Rc::clone(&filter_rc);
            let query_f = Rc::clone(&query_rc);
            search.connect_changed(move |entry| {
                let text = entry.text().to_string();
                *query_f.borrow_mut() = text.clone();
                refresh_list(&list_f, &text, filter_f.get());
            });
        }

        // ---- Keyboard handler (capture phase, same as breadbox) ----
        let key_ctrl = EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        {
            let close_k = Rc::clone(&close_all);
            let list_k = list.clone();
            key_ctrl.connect_key_pressed(move |_, key, _, _| {
                use gtk4::gdk::Key;
                match key {
                    Key::Escape => {
                        close_k();
                        glib::Propagation::Stop
                    }
                    Key::Return | Key::KP_Enter => {
                        if let Some(row) = list_k.selected_row() {
                            if let Some(entry) = get_row_entry(&row) {
                                do_copy(&entry);
                                close_k();
                            }
                        }
                        glib::Propagation::Stop
                    }
                    Key::Delete => {
                        if let Some(row) = list_k.selected_row() {
                            if let Some(entry) = get_row_entry(&row) {
                                if let Ok(db) = HistoryDb::open() {
                                    let _ = db.delete_entry(entry.id);
                                }
                                // Select adjacent row before hiding so focus doesn't vanish
                                let next = list_k
                                    .row_at_index(row.index() + 1)
                                    .filter(|r| r.is_visible())
                                    .or_else(|| {
                                        list_k
                                            .row_at_index((row.index() - 1).max(0))
                                            .filter(|r| r.is_visible())
                                    });
                                list_k.select_row(next.as_ref());
                                row.set_visible(false);
                            }
                        }
                        glib::Propagation::Stop
                    }
                    Key::Down => {
                        let cur = list_k.selected_row().map(|r| r.index()).unwrap_or(-1);
                        let mut i = cur + 1;
                        loop {
                            match list_k.row_at_index(i) {
                                Some(r) if r.is_visible() => {
                                    list_k.select_row(Some(&r));
                                    break;
                                }
                                Some(_) => i += 1,
                                None => break,
                            }
                        }
                        glib::Propagation::Stop
                    }
                    Key::Up => {
                        let cur = list_k.selected_row().map(|r| r.index()).unwrap_or(0);
                        let mut i = cur - 1;
                        loop {
                            if i < 0 {
                                break;
                            }
                            match list_k.row_at_index(i) {
                                Some(r) if r.is_visible() => {
                                    list_k.select_row(Some(&r));
                                    break;
                                }
                                Some(_) => i -= 1,
                                None => break,
                            }
                        }
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        window.add_controller(key_ctrl);

        // ---- Row click ----
        {
            let close_a = Rc::clone(&close_all);
            list.connect_row_activated(move |_, row| {
                if let Some(entry) = get_row_entry(row) {
                    do_copy(&entry);
                    close_a();
                }
            });
        }

        // ---- Click outside panel → close (same pattern as breadbox) ----
        {
            let close_outside = Rc::clone(&close_all);
            let panel_ref = panel.clone();
            let win_ref = window.clone();
            let outside_click = gtk4::GestureClick::new();
            outside_click.connect_pressed(move |_, _, x, y| {
                if let Some(b) = panel_ref.compute_bounds(&win_ref) {
                    let outside = x < b.x() as f64
                        || x > (b.x() + b.width()) as f64
                        || y < b.y() as f64
                        || y > (b.y() + b.height()) as f64;
                    if outside {
                        close_outside();
                    }
                }
            });
            window.add_controller(outside_click);
        }

        window.connect_destroy(|_| cleanup_pid());
        window.present();
        search.grab_focus();
    });

    app.run();
}

// ---- Main -------------------------------------------------------------------

fn main() {
    if !toggle_or_continue() {
        return;
    }

    let entries = HistoryDb::open()
        .and_then(|db| db.list_entries(MAX_ENTRIES))
        .unwrap_or_default();

    run_ui(entries);
}
