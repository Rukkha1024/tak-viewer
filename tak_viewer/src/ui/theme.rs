//! Colors and fonts use a VS Code Dark+ frame and Motive-inspired pane interiors.
//! Values are visually sampled where documented defaults are unavailable.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, Rounding, Stroke};
use tak_core::view_data::ColorSource;

// Panels / chrome: VS Code Default Dark+ (side bar, editor, tabs, status bar). The inside of
// the 3D view and of the panes stays Motive.
pub const PANEL_BG: Color32 = Color32::from_rgb(0x25, 0x25, 0x26);
pub const PANEL_BG_DARK: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
pub const ROW_ALT: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2c);
pub const MENU_BG: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
/// Tab row behind the tabs; a pane title (EXPLORER, LABELS …) sits on it without a tab shape.
pub const TAB_BAR_BG: Color32 = Color32::from_rgb(0x25, 0x25, 0x26);
/// 3D view tabs that are not the shown one of their place.
pub const TAB_BG_INACTIVE: Color32 = Color32::from_rgb(0x2d, 0x2d, 0x2d);
pub const TAB_TEXT_ACTIVE: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const TAB_TEXT_DIM: Color32 = Color32::from_rgb(0x8f, 0x8f, 0x8f);
pub const STATUS_BG: Color32 = Color32::from_rgb(0x00, 0x7a, 0xcc);
pub const STATUS_TEXT: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const HEADER_BG: Color32 = Color32::from_rgb(0x22, 0x22, 0x22);
pub const TEXT: Color32 = Color32::from_rgb(0xd2, 0xd2, 0xd2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8c, 0x8c, 0x8c);
/// Motive accent (cyan headers / active mode).
pub const ACCENT: Color32 = Color32::from_rgb(0x2f, 0xc0, 0xd8);
pub const WARN: Color32 = Color32::from_rgb(0xe8, 0xa3, 0x3d);

// Labels pane: "White label = tracked in the current frame; magenta = not tracked."
pub const LABEL_TRACKED: Color32 = Color32::from_rgb(0xe6, 0xe6, 0xe6);
pub const LABEL_UNTRACKED: Color32 = Color32::from_rgb(0xe0, 0x3c, 0xe0);

// 3D viewport. Settings default background "black"; screenshot ≈ #1c1c1c. Grid default white.
pub const VIEW_BG: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1c);
/// Line on top of the active 3D view's tab, pane borders while dragged (VS Code focus blue).
pub const ACTIVE_VIEW: Color32 = Color32::from_rgb(0x00, 0x7f, 0xd4);
/// Where a dragged pane tab or file would land (ACTIVE_VIEW, about 35% opaque).
pub const DROP_FILL: Color32 = Color32::from_rgba_premultiplied(0x00, 0x2d, 0x4b, 0x5a);
pub const GRID_MINOR: Color32 = Color32::from_rgb(0x38, 0x38, 0x38);
pub const GRID_MAJOR: Color32 = Color32::from_rgb(0x8a, 0x8a, 0x8a);
pub const HUD_TEXT: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xe8);
pub const ASSET_TAG_BG: Color32 = Color32::from_rgb(0x6e, 0x6e, 0x6e);
pub const ASSET_TAG_TEXT: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);

// Axes: triad X red, Y green, Z blue (screenshot); graph lanes use the same hues.
pub const AXIS_X: Color32 = Color32::from_rgb(0xdc, 0x3c, 0x3c);
pub const AXIS_Y: Color32 = Color32::from_rgb(0x4c, 0xd0, 0x3c);
pub const AXIS_Z: Color32 = Color32::from_rgb(0x2a, 0x9c, 0xdc);

// Markers. Settings defaults: passive/unlabeled "white", labeled "white" unless the Color
// visual aid shows the constraint color (file per-marker table), size 14 mm,
// Primary Selection "cyan", Selection (secondary) "yellow".
pub const UNLABELED: Color32 = Color32::from_rgb(0xf0, 0xf0, 0xf0);
pub const LABELED_DEFAULT: Color32 = Color32::from_rgb(0xf0, 0xf0, 0xf0);
pub const SELECT_PRIMARY: Color32 = Color32::from_rgb(0x00, 0xe5, 0xff);
pub const SELECT_SECONDARY: Color32 = Color32::from_rgb(0xff, 0xe8, 0x2a);
pub const NAME_TAG_BG: Color32 = Color32::from_rgba_premultiplied(0x10, 0x10, 0x10, 0xc0);
/// Markers of a file laid over a view (plan step 7): hollow white rings.
pub const OVERLAY_RING: Color32 = Color32::WHITE;
/// Sticks of a file laid over a view (plan step 8): thin white lines.
pub const OVERLAY_STICK: Color32 = Color32::WHITE;
/// Left-drag selection box (implementation choice: faint white fill, light grey edge).
pub const BOX_SELECT_FILL: Color32 = Color32::from_rgba_premultiplied(0x18, 0x18, 0x18, 0x18);
pub const BOX_SELECT_EDGE: Color32 = Color32::from_rgb(0xd0, 0xd0, 0xd0);
/// Fallback when the file has no per-marker color: asset palette (cycled).
pub const ASSET_COLORS: [Color32; 4] = [
    Color32::from_rgb(0x2f, 0xc0, 0xd8),
    Color32::from_rgb(0xe0, 0x4b, 0xe0),
    Color32::from_rgb(0x6c, 0xd8, 0x4a),
    Color32::from_rgb(0xff, 0x9a, 0x3c),
];
pub const EXTRA_TINT: Color32 = Color32::from_rgb(0x96, 0x96, 0x96);
pub const OTHER_PARENT: Color32 = Color32::from_rgb(0xf0, 0xc0, 0x50);
pub const UNKNOWN_KIND: Color32 = Color32::from_rgb(0xa0, 0xa0, 0xa0);

// Force plates.
pub const PLATE_FILL: Color32 = Color32::from_rgba_premultiplied(0x28, 0x30, 0x3a, 0x70);
pub const PLATE_EDGE: Color32 = Color32::from_rgb(0x7a, 0x8a, 0x9e);
pub const PLATE_ACTIVE: Color32 = Color32::from_rgb(0x3d, 0xd6, 0x8c);
/// Edge of the plate chosen in a view (its graph floats over the 3D) and the mark on that
/// graph's name tag (implementation choice: the primary-selection cyan).
pub const PLATE_CHOSEN: Color32 = SELECT_PRIMARY;

// Graph view (Settings default: dark scheme, black background) and navigation bar.
pub const GRAPH_BG: Color32 = Color32::from_rgb(0x24, 0x24, 0x24);
pub const GRAPH_SEP: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
pub const GRAPH_GRID: Color32 = Color32::from_rgb(0x44, 0x44, 0x44);
pub const GRAPH_WATERMARK: Color32 = Color32::from_rgba_premultiplied(0x3a, 0x3a, 0x3a, 0xff);
pub const GRAPH_CURSOR: Color32 = Color32::from_rgb(0xf0, 0xf0, 0xf0);
pub const NAV_BG: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
pub const NAV_GAP: Color32 = Color32::from_rgb(0x8c, 0x22, 0x22);
pub const NAV_GAP_BRIGHT: Color32 = Color32::from_rgb(0xe0, 0x2a, 0x2a);
/// Edit-mode control deck timeline is green (Motive Basics).
pub const TIMELINE_GREEN: Color32 = Color32::from_rgb(0x46, 0xc8, 0x5a);
/// Frame-range band uses a dashed orange outline and faint fill.
pub const RANGE: Color32 = Color32::from_rgb(0xff, 0x98, 0x00);
/// Band strip under the seek zone (a little lighter than `NAV_BG`, so the two zones read apart).
pub const BAND_BG: Color32 = Color32::from_rgb(0x22, 0x22, 0x22);
/// Comment blocks use a solid orange fill, a dark edge, and dark text.
pub const COMMENT: Color32 = RANGE;
pub const COMMENT_EDGE: Color32 = Color32::from_rgb(0x5a, 0x36, 0x00);
pub const COMMENT_TEXT: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1a);
/// The selected comment has a white band edge and a blue row highlight; the current-frame
/// indicator remains a thin orange bar.
pub const COMMENT_PICKED: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const PICKED_ROW: Color32 = Color32::from_rgb(0x09, 0x47, 0x71);
pub const PICKED_EDGE: Color32 = ACTIVE_VIEW;

/// Screen color for a color source recorded by `tak_core::view_data`.
pub fn color(src: ColorSource) -> Color32 {
    match src {
        ColorSource::File(rgb) => rgb_f(rgb),
        ColorSource::Asset(i) => ASSET_COLORS[i % ASSET_COLORS.len()],
        ColorSource::AssetExtra(i) => lerp(ASSET_COLORS[i % ASSET_COLORS.len()], EXTRA_TINT, 0.6),
        ColorSource::OtherParent => OTHER_PARENT,
        ColorSource::Unknown => UNKNOWN_KIND,
        ColorSource::Unlabeled => UNLABELED,
    }
}

pub fn rgb_f(rgb: [f32; 3]) -> Color32 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(c(rgb[0]), c(rgb[1]), c(rgb[2]))
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// Load Malgun Gothic (Hangul) when present; keep egui defaults as fallback.
pub fn install_fonts(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\malgun.ttf",
        "/mnt/c/Windows/Fonts/malgun.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    let mut fonts = FontDefinitions::default();
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert("korean".to_owned(), FontData::from_owned(bytes));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(family).or_default().insert(0, "korean".to_owned());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL_BG;
    visuals.window_fill = PANEL_BG;
    visuals.extreme_bg_color = PANEL_BG_DARK;
    visuals.faint_bg_color = ROW_ALT;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.noninteractive.bg_fill = PANEL_BG;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x3a, 0x3a, 0x3a));
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(0x36, 0x36, 0x36);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x36, 0x36, 0x36);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x46, 0x46, 0x46);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x46, 0x46, 0x46);
    visuals.widgets.active.bg_fill = ACCENT.gamma_multiply(0.7);
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.rounding = Rounding::same(2.0);
    }
    visuals.window_rounding = Rounding::same(3.0);
    // Always dark like Motive, even when Windows is in light mode.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, visuals);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 2.0);
        style.spacing.interact_size.y = 20.0;
    });
}
