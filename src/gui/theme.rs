//! Visual style: light ("Chiaro sistema") and dark ("Grafite") palettes following the system
//! theme, Inter for text and Rajdhani for titles, and the widgets the window is built from:
//! groups of rows with the control on the right, option menus and the burn time slider.
//!
//! Corner radii are concentric: capsules for small controls, 12 for rows and menu items,
//! 16 for groups and menus (12 + their 4 px padding), 20 for windows.

use eframe::egui::{self, Color32, CornerRadius, FontData, FontFamily, FontId, Response, RichText, Stroke};
use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};

/// Colours of one theme.
pub struct Palette {
    /// Window background.
    pub bg: Color32,
    /// Groups and cards.
    pub surface: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    /// The single accent: selection, active values, primary action.
    pub accent: Color32,
    /// Background of unfilled controls: slider rail, buttons, value capsules.
    pub track: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
}

/// "Grafite".
pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x24, 0x24, 0x24),
    surface: Color32::from_rgb(0x36, 0x36, 0x36),
    text: Color32::from_rgb(0xff, 0xff, 0xff),
    text_dim: Color32::from_rgb(0xa8, 0xa8, 0xa8),
    accent: Color32::from_rgb(0x35, 0x84, 0xe4),
    track: Color32::from_rgb(0x55, 0x55, 0x55),
    ok: Color32::from_rgb(0x57, 0xd9, 0x8d),
    warn: Color32::from_rgb(0xff, 0xb4, 0x4d),
    err: Color32::from_rgb(0xff, 0x6b, 0x6b),
};

/// "Chiaro sistema".
pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf5, 0xf5, 0xf7),
    surface: Color32::from_rgb(0xff, 0xff, 0xff),
    text: Color32::from_rgb(0x1d, 0x1d, 0x1f),
    text_dim: Color32::from_rgb(0x6e, 0x6e, 0x73),
    accent: Color32::from_rgb(0x0a, 0x84, 0xff),
    track: Color32::from_rgb(0xd1, 0xd1, 0xd6),
    ok: Color32::from_rgb(0x1a, 0x8f, 0x4c),
    warn: Color32::from_rgb(0xb2, 0x6a, 0x00),
    err: Color32::from_rgb(0xd7, 0x00, 0x15),
};

/// The palette of the theme in use.
pub fn palette(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode { &DARK } else { &LIGHT }
}

pub const ROW_HEIGHT: f32 = 40.0;
pub const CONTROL_HEIGHT: f32 = 28.0;
const TITLE_FAMILY: &str = "title";

/// Font for titles and the name of the application.
pub fn title_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(TITLE_FAMILY.into()))
}

/// `NEJE_GUI_THEME=light|dark` forces a theme for one run; otherwise follow the system.
fn theme_preference() -> egui::ThemePreference {
    match std::env::var("NEJE_GUI_THEME").as_deref() {
        Ok("light") => egui::ThemePreference::Light,
        Ok("dark") => egui::ThemePreference::Dark,
        _ => egui::ThemePreference::System,
    }
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(theme_preference());
    ctx.set_visuals_of(egui::Theme::Dark, visuals(&DARK, egui::Visuals::dark()));
    ctx.set_visuals_of(egui::Theme::Light, visuals(&LIGHT, egui::Visuals::light()));
    ctx.all_styles_mut(|s| {
        use egui::TextStyle;
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.interact_size.y = CONTROL_HEIGHT;
        s.spacing.menu_margin = egui::Margin::same(4);
        s.spacing.scroll = egui::style::ScrollStyle { bar_outer_margin: 4.0, ..egui::style::ScrollStyle::solid() };
        s.text_styles.insert(TextStyle::Heading, title_font(26.0));
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    });
    install_fonts(ctx);
}

fn visuals(p: &Palette, mut v: egui::Visuals) -> egui::Visuals {
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, hairline(p));
    v.window_corner_radius = CornerRadius::same(20);
    v.menu_corner_radius = CornerRadius::same(16);
    v.extreme_bg_color = p.track;
    v.faint_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.track);
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.text_dim);
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.err;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(CONTROL_HEIGHT as u8 / 2);
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke.color = p.text;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, hairline(p));
    v.widgets.noninteractive.fg_stroke.color = p.text_dim;
    v.widgets.inactive.weak_bg_fill = p.track;
    v.widgets.inactive.bg_fill = p.track;
    v.widgets.hovered.weak_bg_fill = p.track.gamma_multiply(if v.dark_mode { 1.3 } else { 0.9 });
    v.widgets.hovered.bg_fill = v.widgets.hovered.weak_bg_fill;
    v.widgets.hovered.expansion = 0.0;
    v.widgets.active.weak_bg_fill = p.accent;
    v.widgets.active.bg_fill = p.accent;
    v.widgets.active.fg_stroke.color = Color32::WHITE;
    v.widgets.active.expansion = 0.0;
    v.widgets.open.weak_bg_fill = p.track;
    v.widgets.open.bg_fill = p.track;
    v
}

/// Separator between rows and the outline of popups.
pub fn hairline(p: &Palette) -> Color32 {
    p.text_dim.gamma_multiply(0.25)
}

/// Inter for everything, Rajdhani for titles; a system font covers the symbols neither has.
fn install_fonts(ctx: &egui::Context) {
    ctx.add_font(FontInsert::new(
        "Inter",
        FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf")),
        vec![InsertFontFamily { family: FontFamily::Proportional, priority: FontPriority::Highest }],
    ));
    ctx.add_font(FontInsert::new(
        "Rajdhani",
        FontData::from_static(include_bytes!("../../assets/fonts/Rajdhani-SemiBold.ttf")),
        vec![InsertFontFamily { family: FontFamily::Name(TITLE_FAMILY.into()), priority: FontPriority::Highest }],
    ));
    install_fallback_fonts(ctx);
}

/// System fonts that cover the symbols the bundled fonts lack (arrows, check marks, dots).
fn install_fallback_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
        "C:\\Windows\\Fonts\\seguisym.ttf",
        "C:\\Windows\\Fonts\\segoeui.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let families = [FontFamily::Proportional, FontFamily::Name(TITLE_FAMILY.into()), FontFamily::Monospace];
            ctx.add_font(FontInsert::new(
                "symbols-fallback",
                FontData::from_owned(bytes),
                families
                    .into_iter()
                    .map(|family| InsertFontFamily { family, priority: FontPriority::Lowest })
                    .collect(),
            ));
            return;
        }
    }
}

/// A group of rows: a small title above and a rounded container.
pub fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    let p = palette(ui);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(RichText::new(title).color(p.text_dim).size(12.5));
    });
    let width = ui.available_width();
    egui::Frame::new().fill(p.surface).corner_radius(CornerRadius::same(16)).inner_margin(egui::Margin::same(4)).show(
        ui,
        |ui| {
            ui.set_width(width - 8.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui);
        },
    );
    ui.add_space(8.0);
}

/// One row of a group: the label on the left, the controls added by `add` on the right
/// (laid out right to left, so the first widget added is the rightmost).
pub fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    let p = palette(ui);
    let first = ui.min_size().y == 0.0;
    let size = egui::vec2(ui.available_width(), ROW_HEIGHT);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(size);
        let rect = ui.max_rect();
        if !first {
            ui.painter().hline((rect.left() + 12.0)..=(rect.right() - 12.0), rect.top(), Stroke::new(1.0, hairline(p)));
        }
        ui.add_space(8.0);
        if !label.is_empty() {
            ui.label(label);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            add(ui);
        });
    });
}

/// A row of equally wide buttons filling the group; returns the index of the clicked one.
/// Each button is (label, enabled, tooltip).
pub fn button_row(ui: &mut egui::Ui, buttons: &[(&str, bool, &str)]) -> Option<usize> {
    let p = palette(ui);
    let first = ui.min_size().y == 0.0;
    let size = egui::vec2(ui.available_width(), ROW_HEIGHT);
    let mut clicked = None;
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(size);
        let rect = ui.max_rect();
        if !first {
            ui.painter().hline((rect.left() + 12.0)..=(rect.right() - 12.0), rect.top(), Stroke::new(1.0, hairline(p)));
        }
        let gap = 6.0;
        ui.spacing_mut().item_spacing.x = gap;
        // Equal widths matter more than side padding: long labels would push the row out.
        ui.spacing_mut().button_padding.x = 4.0;
        ui.add_space(8.0);
        let width = (rect.width() - 16.0 - gap * (buttons.len() as f32 - 1.0)) / buttons.len() as f32;
        for (i, (label, enabled, tooltip)) in buttons.iter().enumerate() {
            let button = egui::Button::new(*label).min_size(egui::vec2(width, CONTROL_HEIGHT));
            let mut response = ui.add_enabled(*enabled, button);
            if !tooltip.is_empty() {
                response = response.on_hover_text(*tooltip).on_disabled_hover_text(*tooltip);
            }
            if response.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Caption in the secondary text colour.
pub fn dim(text: impl Into<String>) -> RichText {
    RichText::new(text).weak().small()
}

/// The primary action of the window: the only accent-filled button.
pub fn primary(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).color(Color32::WHITE)).fill(palette(ui).accent)
}

/// The current value in a capsule that opens a menu with a check mark on the chosen option.
/// Returns the option clicked, when it differs from `selected`.
pub fn choice<T: Clone + PartialEq>(
    ui: &mut egui::Ui,
    id_salt: impl egui::AsIdSalt,
    selected: &T,
    options: &[(T, String)],
) -> Option<T> {
    let p = palette(ui);
    let current = options.iter().find(|(x, _)| x == selected).map_or("—", |(_, name)| name.as_str());
    let mut chosen = None;
    ui.push_id(id_salt, |ui| {
        ui.menu_button(format!("{current}  ⌄"), |ui| {
            ui.set_min_width(170.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            for (x, name) in options {
                let on = x == selected;
                let mark = if on { "✓" } else { " " };
                let text = RichText::new(format!("{name}   {mark}")).color(if on { Color32::WHITE } else { p.text });
                let item = egui::Button::new(text)
                    .fill(if on { p.accent } else { Color32::TRANSPARENT })
                    .corner_radius(CornerRadius::same(12))
                    .min_size(egui::vec2(ui.available_width(), 32.0));
                if ui.add(item).clicked() && !on {
                    chosen = Some(x.clone());
                }
            }
        });
    });
    chosen
}

/// Slider with the value in an editable capsule next to it. `changed()` while dragging,
/// `drag_stopped()` on release, `changed() && !dragged()` when the value is typed.
pub fn slider(ui: &mut egui::Ui, value: &mut u8, min: u8, max: u8, width: f32) -> Response {
    let p = palette(ui);
    let size = egui::vec2(width + 8.0 + 44.0, CONTROL_HEIGHT);
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let (rect, mut rail) = ui.allocate_exact_size(egui::vec2(width, CONTROL_HEIGHT), egui::Sense::click_and_drag());
        let knob = 8.0;
        let span = (rect.left() + knob)..=(rect.right() - knob);
        let (lo, hi) = (f32::from(min), f32::from(max));
        if (rail.dragged() || rail.clicked())
            && let Some(pos) = rail.interact_pointer_pos()
        {
            let new = egui::remap_clamp(pos.x, span.clone(), lo..=hi).round() as u8;
            if new != *value {
                *value = new;
                rail.mark_changed();
            }
        }
        if ui.is_rect_visible(rect) {
            let x = egui::remap_clamp(f32::from(*value), lo..=hi, span);
            let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
            let painter = ui.painter();
            painter.rect_filled(track, 3.0, p.track);
            let filled = egui::Rect::from_min_max(track.min, egui::pos2(x, track.max.y));
            painter.rect_filled(filled, 3.0, fade_if_disabled(ui, p.accent));
            painter.circle(
                egui::pos2(x, rect.center().y),
                knob,
                fade_if_disabled(ui, Color32::WHITE),
                Stroke::new(1.0, Color32::from_black_alpha(60)),
            );
        }
        ui.add_space(8.0);
        let number = ui.add_sized([44.0, CONTROL_HEIGHT], egui::DragValue::new(value).range(min..=max).speed(0.2));
        rail.union(number)
    })
    .inner
}

/// Dims a colour when the ui is disabled.
fn fade_if_disabled(ui: &egui::Ui, color: Color32) -> Color32 {
    if ui.is_enabled() { color } else { color.gamma_multiply(0.5) }
}

/// Switch.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> Response {
    let p = palette(ui);
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(44.0, 24.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(response.id, *on, 0.15);
        let radius = 0.5 * rect.height();
        let fill = fade_if_disabled(ui, if *on { p.accent } else { p.track });
        ui.painter().rect(rect, radius, fill, Stroke::NONE, egui::StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        ui.painter().circle(
            egui::pos2(x, rect.center().y),
            radius - 3.0,
            fade_if_disabled(ui, Color32::WHITE),
            Stroke::new(1.0, Color32::from_black_alpha(40)),
        );
    }
    response
}

/// A dialog over the dimmed window: surface colour, window radius and room around the text.
pub fn modal(name: &'static str, ctx: &egui::Context) -> egui::Modal {
    let p = if ctx.theme() == egui::Theme::Dark { &DARK } else { &LIGHT };
    let frame = egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, hairline(p)))
        .corner_radius(CornerRadius::same(20))
        .inner_margin(egui::Margin::same(20));
    egui::Modal::new(egui::Id::unique(name)).frame(frame)
}
