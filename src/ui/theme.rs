use std::{collections::BTreeMap, sync::Arc};

use egui::{
    style::Selection, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Stroke, Style, TextStyle, Visuals,
};

pub struct Theme;

impl Theme {
    pub const BG_PRIMARY: Color32 = Color32::from_rgb(0x0D, 0x0E, 0x10);
    pub const BG_SECONDARY: Color32 = Color32::from_rgb(0x14, 0x16, 0x1A);
    pub const BG_CARD: Color32 = Color32::from_rgb(0x1A, 0x1D, 0x22);
    pub const ACCENT: Color32 = Color32::from_rgb(222, 88, 51);
    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xDC, 0xE1, 0xE6);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x61, 0x68, 0x75);
    pub const KILL_GREEN: Color32 = Color32::from_rgb(0x1D, 0x9E, 0x75);
    pub const MULTI_PURPLE: Color32 = Color32::from_rgb(0x82, 0x64, 0xD2);
    pub const DEATH_RED: Color32 = Color32::from_rgb(0xC8, 0x3C, 0x3C);
    pub const BORDER: Color32 = Color32::from_rgba_premultiplied(255, 255, 255, 18);

    pub fn apply(ctx: &Context) {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "Ubuntu".to_owned(),
            Arc::new(FontData::from_static(include_bytes!(
                "../../assets/fonts/Ubuntu-Regular.ttf"
            ))),
        );
        fonts.font_data.insert(
            "JetBrains Mono".to_owned(),
            Arc::new(FontData::from_static(include_bytes!(
                "../../assets/fonts/JetBrainsMono-Regular.ttf"
            ))),
        );
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "Ubuntu".to_owned());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "JetBrains Mono".to_owned());
        ctx.set_fonts(fonts);

        let mut style = Style {
            text_styles: BTreeMap::from([
                (TextStyle::Heading, FontId::proportional(26.0)),
                (TextStyle::Body, FontId::proportional(15.0)),
                (TextStyle::Button, FontId::proportional(14.0)),
                (TextStyle::Small, FontId::proportional(12.0)),
                (TextStyle::Monospace, FontId::monospace(13.0)),
            ]),
            ..Style::default()
        };
        style.spacing.item_spacing = egui::vec2(12.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.visuals = Visuals::dark();
        style.visuals.window_fill = Self::BG_PRIMARY;
        style.visuals.panel_fill = Self::BG_PRIMARY;
        style.visuals.extreme_bg_color = Self::BG_PRIMARY;
        style.visuals.faint_bg_color = Self::BG_SECONDARY;
        style.visuals.selection = Selection {
            bg_fill: Self::ACCENT.linear_multiply(0.35),
            stroke: Stroke::new(1.0, Self::ACCENT),
        };
        style.visuals.widgets.noninteractive.bg_fill = Self::BG_CARD;
        style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Self::TEXT_PRIMARY);
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::NONE; // Flat design!
        style.visuals.widgets.inactive.bg_fill = Self::BG_SECONDARY;
        style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Self::TEXT_PRIMARY);
        style.visuals.widgets.inactive.bg_stroke = Stroke::NONE; // Flat design!
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x20, 0x25, 0x2B);
        style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Self::TEXT_PRIMARY);
        style.visuals.widgets.active.bg_fill = Self::ACCENT.linear_multiply(0.8);
        style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, Self::TEXT_PRIMARY);

        let radius = CornerRadius::same(8);
        style.visuals.window_corner_radius = radius;
        style.visuals.menu_corner_radius = radius;
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
        ctx.set_style(style);
    }
}
