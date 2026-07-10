//! Central dark theme for `whisper-tray` (feedback item 1: "make it modern").
//! Builds a custom `egui::Style`/`Visuals` on top of `egui::Visuals::dark()`:
//! rounded corners, comfortable spacing, and a single accent color for
//! sliders/toggles/selected tabs. Pure data construction — no live
//! `egui::Context` is needed to build a [`Style`], so its shape is
//! unit-tested directly; only [`apply`] touches a real context, and that is
//! exercised manually (egui rendering is not unit-tested per project policy).

use egui::{Color32, CornerRadius, Stroke, Style, Vec2, Visuals};

/// Single accent color used for sliders, toggles, selected tabs and active
/// buttons: a calm teal that reads clearly on the dark panel background.
pub const ACCENT: Color32 = Color32::from_rgb(0x4D, 0xB6, 0xAC);

/// Corner radius (px) applied to windows, menus, cards and widgets.
pub const CORNER_RADIUS: u8 = 8;

/// Comfortable spacing between widgets/items.
pub const ITEM_SPACING: Vec2 = Vec2::new(10.0, 10.0);

/// Button content padding.
pub const BUTTON_PADDING: Vec2 = Vec2::new(12.0, 6.0);

/// Build the full [`Style`] for the shell.
#[must_use]
pub fn style() -> Style {
    let mut style = Style {
        visuals: visuals(),
        ..Default::default()
    };
    style.spacing.item_spacing = ITEM_SPACING;
    style.spacing.button_padding = BUTTON_PADDING;
    style
}

/// Build the [`Visuals`] half of the theme (separated so tests can inspect it
/// without the rest of [`Style`]).
#[must_use]
pub fn visuals() -> Visuals {
    let mut v = Visuals::dark();

    let radius = CornerRadius::same(CORNER_RADIUS);
    v.window_corner_radius = radius;
    v.menu_corner_radius = radius;
    v.widgets.noninteractive.corner_radius = radius;
    v.widgets.inactive.corner_radius = radius;
    v.widgets.hovered.corner_radius = radius;
    v.widgets.active.corner_radius = radius;
    v.widgets.open.corner_radius = radius;

    // Subtle panel/window fill split so sidebar and content cards read as
    // separate depth layers rather than a flat form dump.
    v.panel_fill = Color32::from_gray(24);
    v.window_fill = Color32::from_gray(30);
    v.extreme_bg_color = Color32::from_gray(18);

    // Single accent color drives selection, active sliders/toggles/links.
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, Color32::from_gray(10));
    v.hyperlink_color = ACCENT;
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.active.weak_bg_fill = ACCENT;

    v
}

/// A card [`egui::Frame`] for section content: panel-toned fill, inner margin,
/// rounded corners (D-0013 UX feedback: "not a raw form dump").
pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_gray(32))
        .corner_radius(CornerRadius::same(CORNER_RADIUS))
        .inner_margin(egui::Margin::same(12))
        .stroke(Stroke::new(1.0, Color32::from_gray(46)))
}

/// Apply the theme to a live egui context (called once at startup). Not
/// unit-tested (needs a real `Context`); [`style`]/[`visuals`] carry the
/// tested logic.
pub fn apply(ctx: &egui::Context) {
    // Force dark (the design is dark-only) and overwrite both theme slots so
    // an OS light/dark switch never reverts the custom style.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|s| *s = style());
}

/// Colors for the input-level meter fill, banded by loudness so tuning VAD/AGC
/// gives immediate visual feedback (quiet gray -> good green -> hot amber).
pub mod level {
    use egui::Color32;

    pub const QUIET: Color32 = Color32::from_gray(90);
    pub const GOOD: Color32 = Color32::from_rgb(0x4C, 0xAF, 0x50);
    pub const HOT: Color32 = Color32::from_rgb(0xFF, 0xA0, 0x00);

    /// Pick the fill color for an RMS level in `0.0..=1.0`.
    #[must_use]
    pub fn color_for_rms(rms: f32) -> Color32 {
        if rms < 0.05 {
            QUIET
        } else if rms < 0.6 {
            GOOD
        } else {
            HOT
        }
    }
}

/// Colors + labels for the connection/engine state pill (D-0013 feedback:
/// "colored dot + label").
pub mod status_pill {
    use crate::tray_state::TrayState;
    use egui::Color32;

    /// The pill's displayed state — a superset of [`TrayState`] that also
    /// distinguishes "connected & idle" from "disconnected".
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PillState {
        Connected,
        Recording,
        Processing,
        Error,
        Disconnected,
    }

    impl PillState {
        #[must_use]
        pub fn label(self) -> &'static str {
            match self {
                PillState::Connected => "Connected",
                PillState::Recording => "Recording",
                PillState::Processing => "Processing",
                PillState::Error => "Error",
                PillState::Disconnected => "Disconnected",
            }
        }

        #[must_use]
        pub fn color(self) -> Color32 {
            match self {
                PillState::Connected => Color32::from_rgb(0x4C, 0xAF, 0x50),
                PillState::Recording => Color32::from_rgb(0xE0, 0x3B, 0x24),
                PillState::Processing => Color32::from_rgb(0xF2, 0x9D, 0x1C),
                PillState::Error => Color32::from_rgb(0x8B, 0x1A, 0x1A),
                PillState::Disconnected => Color32::from_gray(0x60),
            }
        }
    }

    /// Derive the pill state from the shell's WS connection flag and tray
    /// state (pure so it is unit-tested without a GUI).
    #[must_use]
    pub fn from_tray(connected: bool, tray: TrayState) -> PillState {
        if !connected {
            return PillState::Disconnected;
        }
        match tray {
            TrayState::Idle => PillState::Connected,
            TrayState::Recording => PillState::Recording,
            TrayState::Processing => PillState::Processing,
            TrayState::Error => PillState::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray_state::TrayState;

    #[test]
    fn visuals_apply_rounded_corners() {
        let v = visuals();
        let radius = CornerRadius::same(CORNER_RADIUS);
        assert_eq!(v.window_corner_radius, radius);
        assert_eq!(v.widgets.active.corner_radius, radius);
        assert_eq!(v.widgets.inactive.corner_radius, radius);
    }

    #[test]
    fn visuals_use_a_single_accent_for_selection_and_active_widgets() {
        let v = visuals();
        assert_eq!(v.selection.bg_fill, ACCENT);
        assert_eq!(v.widgets.active.bg_fill, ACCENT);
        assert_eq!(v.hyperlink_color, ACCENT);
    }

    #[test]
    fn style_carries_comfortable_spacing() {
        let s = style();
        assert_eq!(s.spacing.item_spacing, ITEM_SPACING);
        assert_eq!(s.spacing.button_padding, BUTTON_PADDING);
    }

    #[test]
    fn card_frame_has_rounding_and_margin() {
        let f = card_frame();
        assert_eq!(f.corner_radius, CornerRadius::same(CORNER_RADIUS));
        assert_eq!(f.inner_margin, egui::Margin::same(12));
    }

    #[test]
    fn level_color_bands_quiet_good_hot() {
        assert_eq!(level::color_for_rms(0.0), level::QUIET);
        assert_eq!(level::color_for_rms(0.3), level::GOOD);
        assert_eq!(level::color_for_rms(0.9), level::HOT);
    }

    #[test]
    fn pill_disconnected_overrides_tray_state() {
        assert_eq!(
            status_pill::from_tray(false, TrayState::Recording),
            status_pill::PillState::Disconnected
        );
    }

    #[test]
    fn pill_maps_connected_tray_states() {
        assert_eq!(
            status_pill::from_tray(true, TrayState::Idle),
            status_pill::PillState::Connected
        );
        assert_eq!(
            status_pill::from_tray(true, TrayState::Recording),
            status_pill::PillState::Recording
        );
        assert_eq!(
            status_pill::from_tray(true, TrayState::Processing),
            status_pill::PillState::Processing
        );
        assert_eq!(
            status_pill::from_tray(true, TrayState::Error),
            status_pill::PillState::Error
        );
    }

    #[test]
    fn pill_labels_are_short_and_english() {
        for (state, label) in [
            (status_pill::PillState::Connected, "Connected"),
            (status_pill::PillState::Recording, "Recording"),
            (status_pill::PillState::Processing, "Processing"),
            (status_pill::PillState::Error, "Error"),
            (status_pill::PillState::Disconnected, "Disconnected"),
        ] {
            assert_eq!(state.label(), label);
        }
    }
}
