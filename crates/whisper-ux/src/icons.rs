//! Programmatically rasterized menu-bar icons (D-0013 §2). No image assets: each
//! state is a filled, anti-aliased disc in RGBA8, so there is nothing to bundle
//! and the buffers are trivially unit-tested. `tray_icon::Icon::from_rgba` takes
//! exactly this `(rgba, width, height)` shape.

use crate::tray_state::TrayState;

/// Side length in pixels of the generated icon (square). NSStatusItem scales it;
/// 32px looks crisp at the menu-bar's ~18pt on Retina.
pub const ICON_SIZE: u32 = 32;

/// RGBA8 pixels plus dimensions, ready for `tray_icon::Icon::from_rgba`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// macOS "template" images are recolored by the system for light/dark menu
    /// bars. Only the monochrome idle glyph is a template; the colored state
    /// discs (red/orange) must keep their color, so they are not.
    pub template: bool,
}

/// The disc color for a state, as opaque RGB. Idle is a neutral gray glyph that
/// works as a dark-mode template; recording is red, processing orange, error a
/// darker red.
fn state_rgb(state: TrayState) -> [u8; 3] {
    match state {
        TrayState::Idle => [0x88, 0x88, 0x88],
        TrayState::Recording => [0xE0, 0x3B, 0x24],
        TrayState::Processing => [0xF2, 0x9D, 0x1C],
        TrayState::Error => [0x8B, 0x1A, 0x1A],
    }
}

/// Render the icon for `state` at [`ICON_SIZE`].
#[must_use]
pub fn render(state: TrayState) -> IconImage {
    render_disc(
        ICON_SIZE,
        state_rgb(state),
        matches!(state, TrayState::Idle),
    )
}

/// Rasterize a filled disc of `rgb` on a transparent background, with 1px of
/// edge anti-aliasing via coverage sampling.
fn render_disc(size: u32, rgb: [u8; 3], template: bool) -> IconImage {
    let n = size as usize;
    let mut rgba = vec![0u8; n * n * 4];
    let center = (size as f32 - 1.0) / 2.0;
    // Leave a 2px margin so the disc isn't clipped at the bounds.
    let radius = center - 1.0;

    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            // Coverage: 1.0 well inside, 0.0 well outside, linear across a 1px band.
            let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0);
            let alpha = (coverage * 255.0).round() as u8;
            let i = (y * n + x) * 4;
            rgba[i] = rgb[0];
            rgba[i + 1] = rgb[1];
            rgba[i + 2] = rgb[2];
            rgba[i + 3] = alpha;
        }
    }

    IconImage {
        rgba,
        width: size,
        height: size,
        template,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(img: &IconImage, x: u32, y: u32) -> u8 {
        let i = ((y * img.width + x) * 4) as usize;
        img.rgba[i + 3]
    }

    #[test]
    fn buffer_has_rgba_length_and_dimensions() {
        let img = render(TrayState::Idle);
        assert_eq!(img.width, ICON_SIZE);
        assert_eq!(img.height, ICON_SIZE);
        assert_eq!(img.rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    }

    #[test]
    fn center_is_opaque_and_corner_is_transparent() {
        for state in [
            TrayState::Idle,
            TrayState::Recording,
            TrayState::Processing,
            TrayState::Error,
        ] {
            let img = render(state);
            assert_eq!(
                alpha_at(&img, ICON_SIZE / 2, ICON_SIZE / 2),
                255,
                "{state:?}"
            );
            assert_eq!(alpha_at(&img, 0, 0), 0, "{state:?}");
        }
    }

    #[test]
    fn center_pixel_carries_state_color() {
        let img = render(TrayState::Recording);
        let i = (((ICON_SIZE / 2) * img.width + ICON_SIZE / 2) * 4) as usize;
        assert_eq!(&img.rgba[i..i + 3], &state_rgb(TrayState::Recording));
    }

    #[test]
    fn only_idle_is_a_template() {
        assert!(render(TrayState::Idle).template);
        assert!(!render(TrayState::Recording).template);
        assert!(!render(TrayState::Processing).template);
        assert!(!render(TrayState::Error).template);
    }

    #[test]
    fn states_are_visually_distinct() {
        let idle = render(TrayState::Idle);
        let rec = render(TrayState::Recording);
        assert_ne!(idle.rgba, rec.rgba);
    }
}
