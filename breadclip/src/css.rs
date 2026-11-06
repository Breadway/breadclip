use bread_theme::{hex_to_rgba, ink_on, tokens::*, Palette};

pub fn build_css(p: &Palette) -> String {
    let bg_panel = hex_to_rgba(&p.background, 0.80);
    let surface = hex_to_rgba(&p.color0, 0.85);
    let on_bg = ink_on(&p.background);
    let on_surface = ink_on(&p.color0);
    let accent = &p.color4;
    let on_accent = ink_on(&p.color4);
    let overlay = &p.color7;
    let on_overlay = ink_on(&p.color7);

    // `window` must be transparent so Hyprland's `layerrule = blur` can render
    // the frosted effect behind the panel. The panel itself carries the 80% fill.
    //
    // Row colours are set on `.clip-row` (not the shared `row` selector) so
    // they override the shared `row:selected { background: @accent }` rule and
    // use the muted surface tone instead, consistent with breadbox.
    //
    // `.clip-thumbnail` only sets a 48px floor here. GTK4 CSS has no
    // `max-width`/`max-height` (they used to log "No property named" parser
    // warnings on every start and were ignored); the matching hard cap is a
    // fixed-size overlay around the Picture, see `build_thumbnail`.
    format!(
        "window {{ background-color: transparent; }}\
         .clip-panel {{\
             background-color: {bg_panel};\
             color: {on_bg};\
             border-radius: {r1}px;\
             box-shadow: 0 8px 32px rgba(0,0,0,0.6);\
         }}\
         .clip-row {{\
             background-color: transparent;\
             border-radius: {r2}px;\
             padding: {sm}px {md}px;\
             color: {on_bg};\
         }}\
         .clip-row:hover {{\
             background-color: {surface};\
             color: {on_surface};\
         }}\
         .clip-row:selected {{\
             background-color: {surface};\
             color: {on_surface};\
         }}\
         .clip-text-preview {{\
             font-size: {base}px;\
         }}\
         .clip-meta {{\
             font-size: {sec}px;\
             opacity: 0.6;\
         }}\
         .clip-thumbnail {{\
             min-width: 48px;\
             min-height: 48px;\
         }}\
         .chips-row {{\
             padding: {xs}px {sm}px;\
         }}\
         .chip {{\
             background-color: {overlay};\
             color: {on_overlay};\
             border: none;\
             background-image: none;\
             box-shadow: none;\
             outline: none;\
             border-radius: 999px;\
             padding: {xs}px {md}px;\
             font-size: {sec}px;\
         }}\
         .chip:hover {{\
             background-color: {overlay};\
             background-image: none;\
             box-shadow: none;\
         }}\
         .chip.active {{\
             background-color: {accent};\
             color: {on_accent};\
             background-image: none;\
             box-shadow: none;\
         }}\
         .chip.active:hover {{\
             background-color: {accent};\
             background-image: none;\
             box-shadow: none;\
         }}\
         searchentry {{\
             background-color: {surface_raw};\
             color: {on_surface};\
             caret-color: {accent};\
             border: none;\
             outline: none;\
             box-shadow: none;\
             padding: {md}px {lg}px;\
             border-radius: {r1}px {r1}px 0 0;\
         }}\
         listbox {{\
             background-color: transparent;\
             padding: {xs}px;\
         }}\
         row {{ padding: 0; }}\
         row:selected {{ background-color: transparent; }}\
         row:hover {{ background-color: transparent; }}",
        bg_panel = bg_panel,
        surface = surface,
        surface_raw = p.color0,
        on_bg = on_bg,
        on_surface = on_surface,
        accent = accent,
        on_accent = on_accent,
        overlay = overlay,
        on_overlay = on_overlay,
        base = FONT_SIZE_BASE,
        sec = FONT_SIZE_SECONDARY,
        xs = SPACE_XS,
        sm = SPACE_SM,
        md = SPACE_MD,
        lg = SPACE_LG,
        r1 = RADIUS_PRIMARY,
        r2 = RADIUS_SECONDARY,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    /// Load `css` through a real `GtkCssProvider` and return every parsing
    /// error GTK reports.
    ///
    /// `CssProvider::new()` asserts that `gtk::init()` ran, which needs a
    /// display. CSS parsing is pure type-system work with no display
    /// dependency, so construct the GObject through glib directly; that keeps
    /// this test headless (CI runs `cargo test` in a container with no
    /// compositor).
    fn parse_errors(css: &str) -> Vec<String> {
        let provider: gtk4::CssProvider = gtk4::glib::Object::new();
        let errors: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let errors = Rc::clone(&errors);
            provider.connect_parsing_error(move |_, section, error| {
                let start = section.start_location();
                let end = section.end_location();
                errors.borrow_mut().push(format!(
                    "{}:{}-{}: {}",
                    start.lines() + 1,
                    start.line_chars(),
                    end.line_chars(),
                    error
                ));
            });
        }
        provider.load_from_string(css);
        let errors = errors.borrow().clone();
        errors
    }

    fn assert_clean(name: &str, css: &str) {
        let errors = parse_errors(css);
        assert!(
            errors.is_empty(),
            "{name} stylesheet has GTK CSS parse errors:\n{}",
            errors.join("\n")
        );
    }

    /// CLIP-25: `max-width`/`max-height` are not GTK4 CSS properties. Every
    /// start logged "No property named" for them and silently dropped the
    /// intended size limit. Load every sheet the popup applies so an invalid
    /// declaration fails here instead of only showing up as a warning at
    /// runtime.
    ///
    /// Kept as one test on purpose: GTK's type system and CSS parser are not
    /// safe to drive from several test threads at once, and parallel
    /// `GtkCssProvider` construction without `gtk::init()` segfaults.
    #[test]
    fn stylesheets_parse_without_errors() {
        assert_clean("breadclip", &build_css(&Palette::default()));
        // The app layers the shared bread-theme sheet under its own; a bad
        // property there produces the same startup warnings.
        assert_clean("bread-theme", &bread_theme::render());
        // Guard the harness itself. If GTK ever stops routing parser failures
        // to the signal (or the headless construction path swallows them),
        // the checks above would pass vacuously; this one fails instead.
        assert!(
            !parse_errors(".x { max-width: 48px; }").is_empty(),
            "GTK did not report max-width as an invalid property"
        );
    }
}
