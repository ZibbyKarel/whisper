//! Programmatically rasterized menu-bar icons (D-0013 §2). No image assets:
//! each state renders a 5-bar waveform glyph into an RGBA8 buffer, so there
//! is nothing to bundle and the buffers are trivially unit-tested.
//! `tray_icon::Icon::from_rgba` takes exactly this `(rgba, width, height)`
//! shape.

use crate::tray_state::TrayState;

/// Side length in pixels of the generated icon (square). NSStatusItem scales it;
/// 32px looks crisp at the menu-bar's ~18pt on Retina.
pub const ICON_SIZE: u32 = 32;

/// Per-axis supersampling factor for edge anti-aliasing (16 samples/px total).
const SUPERSAMPLE: u32 = 4;

/// Waveform bar heights as a fraction of the glyph's drawable height, in
/// left-to-right order: a calm "voice" silhouette (short-tall-tallest-tall-short).
const BAR_HEIGHT_FRACTIONS: [f32; 5] = [0.40, 0.70, 1.00, 0.70, 0.40];

/// Distinct alert color for the error-state corner badge (a saturated red,
/// deliberately different from the amber waveform so it pops as an "alert").
const ERROR_BADGE_RGB: [u8; 3] = [0xC6, 0x28, 0x28];

/// RGBA8 pixels plus dimensions, ready for `tray_icon::Icon::from_rgba`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// macOS "template" images are recolored by the system for light/dark menu
    /// bars. No state currently uses this: idle is an explicit solid white
    /// glyph (matching the app's dark theme) and the other states carry their
    /// own colors (red/teal/amber), so all icons keep their literal RGB.
    pub template: bool,
}

/// Per-state rendering parameters for the waveform glyph.
struct WaveformStyle {
    /// Bar (and badge, if any) fill color.
    rgb: [u8; 3],
    /// Alpha multiplier for fully-covered bar pixels, 0.0..=1.0.
    opacity: f32,
    /// Bar width multiplier vs. the base bar width (>1.0 = bolder).
    weight: f32,
    /// Bar height multiplier vs. the base glyph height (>1.0 = taller).
    height_scale: f32,
    /// Whether this is the macOS "template" (monochrome, alpha-only) glyph.
    template: bool,
    /// Whether to draw the small alert badge in the bottom-right corner.
    badge: bool,
}

fn style_for(state: TrayState) -> WaveformStyle {
    match state {
        // Idle: an explicit solid white glyph, matching the app's dark theme.
        // Not a template image, so it trades away automatic light/dark
        // menu-bar recoloring for a guaranteed-visible white waveform.
        TrayState::Idle => WaveformStyle {
            rgb: [0xFF, 0xFF, 0xFF],
            opacity: 1.0,
            weight: 1.0,
            height_scale: 1.0,
            template: false,
            badge: false,
        },
        // Recording: red, bolder and taller bars so the active state reads
        // at a glance.
        TrayState::Recording => WaveformStyle {
            rgb: [0xE5, 0x48, 0x4D],
            opacity: 1.0,
            weight: 1.15,
            height_scale: 1.08,
            template: false,
            badge: false,
        },
        // Processing: theme teal accent (matches theme::ACCENT) at ~70%
        // opacity to read as transient/in-progress.
        TrayState::Processing => WaveformStyle {
            rgb: [0x4D, 0xB6, 0xAC],
            opacity: 0.70,
            weight: 1.0,
            height_scale: 1.0,
            template: false,
            badge: false,
        },
        // Error: amber/orange waveform plus a small red alert dot badge.
        TrayState::Error => WaveformStyle {
            rgb: [0xF2, 0x9D, 0x1C],
            opacity: 1.0,
            weight: 1.0,
            height_scale: 1.0,
            template: false,
            badge: true,
        },
    }
}

/// Render the icon for `state` at [`ICON_SIZE`].
#[must_use]
pub fn render(state: TrayState) -> IconImage {
    render_waveform(ICON_SIZE, &style_for(state))
}

/// Geometry of the 5 waveform bars for a given canvas size: `(glyph_w,
/// glyph_h, left_x, center_y, base_bar_w, pitch)`. Pure function of `sz` so
/// tests can recompute the same layout.
fn bar_geometry(sz: f32) -> (f32, f32, f32, f32, f32, f32) {
    let glyph_w = sz * 0.66;
    let glyph_h = sz * 0.62;
    let left_x = (sz - glyph_w) / 2.0;
    let center_y = sz / 2.0;

    let bar_count = BAR_HEIGHT_FRACTIONS.len() as f32;
    // Gap wide enough that even the bolder (weight-scaled) recording bars
    // still leave a fully-transparent column between neighbors.
    let gap_ratio = 0.7; // gap = 0.7 * base bar width
    let base_bar_w = glyph_w / (bar_count + (bar_count - 1.0) * gap_ratio);
    let pitch = base_bar_w * (1.0 + gap_ratio);

    (glyph_w, glyph_h, left_x, center_y, base_bar_w, pitch)
}

/// Center + radius of the corner alert badge, bottom-right of the canvas,
/// clear of the waveform glyph's bounding box.
fn badge_geometry(sz: f32) -> (f32, f32, f32) {
    let cx = sz - sz * 0.16;
    let cy = sz - sz * 0.16;
    let r = sz * 0.11;
    (cx, cy, r)
}

/// The five `(center_x, center_y, height)` bars for `style` at canvas size `sz`.
fn bars_for(sz: f32, style: &WaveformStyle) -> [(f32, f32, f32); 5] {
    let (_glyph_w, glyph_h, left_x, center_y, base_bar_w, pitch) = bar_geometry(sz);
    let mut bars = [(0.0f32, 0.0f32, 0.0f32); 5];
    for (i, frac) in BAR_HEIGHT_FRACTIONS.iter().enumerate() {
        let cx = left_x + base_bar_w / 2.0 + (i as f32) * pitch;
        let h = glyph_h * frac * style.height_scale;
        bars[i] = (cx, center_y, h);
    }
    bars
}

/// True if point `(px, py)` lies within a capsule (a rectangle of width `w`
/// and height `h`, with semicircular caps of radius `w / 2`) centered at
/// `(cx, cy)`. `w` is expected to be `<= h` (tall, narrow bars).
fn inside_capsule(px: f32, py: f32, cx: f32, cy: f32, w: f32, h: f32) -> bool {
    let r = w / 2.0;
    let dx = ((px - cx).abs() - (w / 2.0 - r)).max(0.0);
    let dy = ((py - cy).abs() - (h / 2.0 - r)).max(0.0);
    (dx * dx + dy * dy).sqrt() <= r
}

/// True if point `(px, py)` lies within a circle of radius `r` centered at
/// `(cx, cy)`.
fn inside_dot(px: f32, py: f32, cx: f32, cy: f32, r: f32) -> bool {
    let dx = px - cx;
    let dy = py - cy;
    (dx * dx + dy * dy).sqrt() <= r
}

/// Rasterize the waveform glyph for `style` on a transparent background,
/// with `SUPERSAMPLE`×`SUPERSAMPLE` coverage-based anti-aliasing so bar edges
/// (straight sides + rounded caps) don't look jagged at menu-bar size.
fn render_waveform(size: u32, style: &WaveformStyle) -> IconImage {
    let n = size as usize;
    let sz = size as f32;
    let mut rgba = vec![0u8; n * n * 4];

    let bars = bars_for(sz, style);
    let (_, _, _, _, base_bar_w, _) = bar_geometry(sz);
    let bar_w = base_bar_w * style.weight;
    let (badge_cx, badge_cy, badge_r) = badge_geometry(sz);

    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    let step = 1.0 / SUPERSAMPLE as f32;

    for y in 0..n {
        for x in 0..n {
            let mut bar_hits = 0u32;
            let mut badge_hits = 0u32;

            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let px = x as f32 + (sx as f32 + 0.5) * step;
                    let py = y as f32 + (sy as f32 + 0.5) * step;

                    if bars
                        .iter()
                        .any(|&(bcx, bcy, bh)| inside_capsule(px, py, bcx, bcy, bar_w, bh))
                    {
                        bar_hits += 1;
                    } else if style.badge && inside_dot(px, py, badge_cx, badge_cy, badge_r) {
                        badge_hits += 1;
                    }
                }
            }

            let bar_coverage = bar_hits as f32 / samples;
            let badge_coverage = badge_hits as f32 / samples;
            let i = (y * n + x) * 4;

            if bar_coverage > 0.0 {
                let alpha = (bar_coverage * 255.0 * style.opacity).round() as u8;
                rgba[i] = style.rgb[0];
                rgba[i + 1] = style.rgb[1];
                rgba[i + 2] = style.rgb[2];
                rgba[i + 3] = alpha;
            } else if badge_coverage > 0.0 {
                let alpha = (badge_coverage * 255.0).round() as u8;
                rgba[i] = ERROR_BADGE_RGB[0];
                rgba[i + 1] = ERROR_BADGE_RGB[1];
                rgba[i + 2] = ERROR_BADGE_RGB[2];
                rgba[i + 3] = alpha;
            }
        }
    }

    IconImage {
        rgba,
        width: size,
        height: size,
        template: style.template,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(img: &IconImage, x: u32, y: u32) -> u8 {
        let i = ((y * img.width + x) * 4) as usize;
        img.rgba[i + 3]
    }

    fn rgb_at(img: &IconImage, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * img.width + x) * 4) as usize;
        [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
    }

    /// Count contiguous horizontal runs of majority-opaque pixels in row `y`.
    /// The waveform is 5 separate bars each ~3px wide with a ~1.5px gap, so a
    /// scanline through their shared vertical center should cross exactly 5
    /// runs. Pixel columns straddling a bar edge carry partial (AA) alpha
    /// even inside the gap, so "majority opaque" (alpha > 127) — not merely
    /// non-zero — is the right notion of "inside a bar" here.
    fn horizontal_runs(img: &IconImage, y: u32) -> usize {
        let mut runs = 0;
        let mut in_run = false;
        for x in 0..img.width {
            let opaque = alpha_at(img, x, y) > 127;
            if opaque && !in_run {
                runs += 1;
            }
            in_run = opaque;
        }
        runs
    }

    #[test]
    fn buffer_has_rgba_length_and_dimensions() {
        let img = render(TrayState::Idle);
        assert_eq!(img.width, ICON_SIZE);
        assert_eq!(img.height, ICON_SIZE);
        assert_eq!(img.rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    }

    #[test]
    fn buffer_is_non_empty_for_every_state() {
        for state in [
            TrayState::Idle,
            TrayState::Recording,
            TrayState::Processing,
            TrayState::Error,
        ] {
            let img = render(state);
            assert!(
                img.rgba.iter().any(|&b| b != 0),
                "{state:?} buffer is entirely empty"
            );
        }
    }

    #[test]
    fn waveform_renders_five_distinct_bars() {
        // Scan the row through the bars' shared vertical center; badge (if
        // any) lives well below this row, so it can't add a spurious run.
        let center_row = ICON_SIZE / 2;
        for state in [
            TrayState::Idle,
            TrayState::Recording,
            TrayState::Processing,
            TrayState::Error,
        ] {
            let img = render(state);
            assert_eq!(
                horizontal_runs(&img, center_row),
                5,
                "{state:?} should show 5 separate bars at the center row"
            );
        }
    }

    #[test]
    fn tallest_bar_reaches_full_coverage_somewhere() {
        for state in [TrayState::Idle, TrayState::Recording, TrayState::Error] {
            let img = render(state);
            assert!(
                img.rgba.chunks_exact(4).any(|p| p[3] == 255),
                "{state:?} should have at least one fully-opaque pixel"
            );
        }
    }

    #[test]
    fn no_state_is_a_template_icon() {
        assert!(!render(TrayState::Idle).template);
        assert!(!render(TrayState::Recording).template);
        assert!(!render(TrayState::Processing).template);
        assert!(!render(TrayState::Error).template);
    }

    #[test]
    fn idle_is_solid_white() {
        let img = render(TrayState::Idle);
        for px in img.rgba.chunks_exact(4) {
            if px[3] == 255 {
                assert_eq!(
                    [px[0], px[1], px[2]],
                    [0xFF, 0xFF, 0xFF],
                    "idle glyph's opaque pixels must be pure white"
                );
            }
        }
    }

    #[test]
    fn recording_pixels_are_red_dominant() {
        let img = render(TrayState::Recording);
        let mut saw_opaque = false;
        for px in img.rgba.chunks_exact(4) {
            if px[3] > 0 {
                saw_opaque = true;
                assert!(
                    px[0] >= px[1] && px[0] >= px[2],
                    "expected red-dominant pixel {px:?}"
                );
            }
        }
        assert!(saw_opaque);
    }

    #[test]
    fn processing_uses_teal_accent_at_reduced_opacity() {
        let img = render(TrayState::Processing);
        let max_alpha = img.rgba.chunks_exact(4).map(|p| p[3]).max().unwrap();
        assert!(max_alpha > 0, "processing glyph must not be empty");
        // 70% opacity ceiling, with a little slack for AA rounding.
        assert!(
            max_alpha <= 180,
            "processing max alpha {max_alpha} exceeds ~70% opacity"
        );
        // Every opaque pixel should carry the teal accent color.
        for px in img.rgba.chunks_exact(4) {
            if px[3] > 0 {
                assert_eq!([px[0], px[1], px[2]], [0x4D, 0xB6, 0xAC]);
            }
        }
    }

    #[test]
    fn error_has_amber_waveform_and_red_badge() {
        let img = render(TrayState::Error);
        let (bx, by, _) = badge_geometry(ICON_SIZE as f32);
        let badge_pixel = rgb_at(&img, bx.round() as u32, by.round() as u32);
        assert_eq!(
            badge_pixel, ERROR_BADGE_RGB,
            "badge center pixel should carry the alert-red badge color"
        );
        assert!(alpha_at(&img, bx.round() as u32, by.round() as u32) > 0);

        // The waveform itself (top of the tallest, center bar) stays amber.
        let center = ICON_SIZE / 2;
        assert_eq!(rgb_at(&img, center, center), [0xF2, 0x9D, 0x1C]);
    }

    #[test]
    fn recording_bars_are_bolder_than_idle() {
        // Bolder bars cover more pixels in the center row than the base weight.
        let idle_count = render(TrayState::Idle)
            .rgba
            .chunks_exact(4)
            .filter(|p| p[3] > 0)
            .count();
        let recording_count = render(TrayState::Recording)
            .rgba
            .chunks_exact(4)
            .filter(|p| p[3] > 0)
            .count();
        assert!(
            recording_count > idle_count,
            "recording ({recording_count}) should cover more pixels than idle ({idle_count})"
        );
    }

    #[test]
    fn states_are_visually_distinct() {
        let idle = render(TrayState::Idle);
        let rec = render(TrayState::Recording);
        let processing = render(TrayState::Processing);
        let error = render(TrayState::Error);
        assert_ne!(idle.rgba, rec.rgba);
        assert_ne!(idle.rgba, processing.rgba);
        assert_ne!(idle.rgba, error.rgba);
        assert_ne!(rec.rgba, processing.rgba);
        assert_ne!(rec.rgba, error.rgba);
        assert_ne!(processing.rgba, error.rgba);
    }

    #[test]
    fn render_is_deterministic() {
        for state in [
            TrayState::Idle,
            TrayState::Recording,
            TrayState::Processing,
            TrayState::Error,
        ] {
            assert_eq!(render(state), render(state));
        }
    }
}
