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
             max-width: 48px;\
             max-height: 48px;\
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
