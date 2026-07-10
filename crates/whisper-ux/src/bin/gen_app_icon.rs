//! Dev tool: renders `Whisper.app`'s macOS icon programmatically (no source
//! art) and writes the full iconset of PNGs macOS's `iconutil` needs to build
//! `assets/AppIcon.icns`.
//!
//! Design (Big Sur+ style, D-0013/D-0014 iconography):
//! - A rounded square (~22.5% corner radius) filling most of the canvas.
//! - A vertical gradient from deep teal (`#0E3B39`) to the shell's teal
//!   accent (`#4DB6AC`, `theme::ACCENT`).
//! - The same 5-bar white waveform glyph used in the menu-bar icon
//!   ([`whisper_ux::icons`]), centered, with a soft shadow beneath it.
//!
//! Deterministic: no timestamps, randomness, or filesystem-order dependence —
//! re-running produces byte-identical PNGs.
//!
//! Usage: `cargo run -p whisper-ux --bin gen_app_icon`. Writes
//! `target/AppIcon.iconset/*.png`; run `iconutil -c icns` on that directory
//! to produce `assets/AppIcon.icns` (this binary does not shell out).

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

/// Per-axis supersampling factor for edge/gradient anti-aliasing.
const SUPERSAMPLE: u32 = 4;

/// Corner radius as a fraction of the square's side length (Big Sur-style).
const CORNER_RADIUS_FRAC: f32 = 0.225;
/// Inset of the rounded square from the canvas edge, as a fraction of size.
const SQUARE_INSET_FRAC: f32 = 0.02;

/// Gradient endpoints: deep teal (top) -> accent teal (bottom, matches
/// `theme::ACCENT`).
const GRADIENT_TOP: [u8; 3] = [0x0E, 0x3B, 0x39];
const GRADIENT_BOTTOM: [u8; 3] = [0x4D, 0xB6, 0xAC];

/// Waveform bar heights, same silhouette as the tray glyph (`icons.rs`).
const BAR_HEIGHT_FRACTIONS: [f32; 5] = [0.40, 0.70, 1.00, 0.70, 0.40];
const GLYPH_WIDTH_FRAC: f32 = 0.46;
const GLYPH_HEIGHT_FRAC: f32 = 0.42;
const GAP_RATIO: f32 = 0.6;

const SHADOW_OFFSET_FRAC: f32 = 0.012;
const SHADOW_STRENGTH: f32 = 0.35;

/// `(filename, pixel size)` for every PNG macOS's `iconutil` expects in an
/// `.iconset` bundle: 10 filenames covering 7 distinct pixel sizes (some
/// `@2x` variants share a size with a plain-named entry, e.g.
/// `icon_16x16@2x.png` and `icon_32x32.png` are both 32x32).
const ICONSET_ENTRIES: [(&str, u32); 10] = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
];

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent() // crates/
        .and_then(Path::parent) // repo root
        .expect("whisper-ux is always two directories under the repo root")
        .to_path_buf();

    let iconset_dir = repo_root.join("target").join("AppIcon.iconset");
    fs::create_dir_all(&iconset_dir).expect("create target/AppIcon.iconset");

    // Render each distinct pixel size once; several iconset filenames share a
    // size (e.g. icon_16x16@2x.png and icon_32x32.png are both 32x32), so
    // this both saves work and guarantees byte-identical duplicates.
    let mut distinct_sizes: Vec<u32> = ICONSET_ENTRIES.iter().map(|&(_, s)| s).collect();
    distinct_sizes.sort_unstable();
    distinct_sizes.dedup();

    let rendered: Vec<(u32, Vec<u8>)> = distinct_sizes
        .into_iter()
        .map(|size| (size, render_app_icon(size)))
        .collect();

    for &(name, size) in &ICONSET_ENTRIES {
        let rgba = &rendered
            .iter()
            .find(|(s, _)| *s == size)
            .expect("every requested size was rendered")
            .1;
        let path = iconset_dir.join(name);
        write_png(&path, size, size, rgba).unwrap_or_else(|e| panic!("write {path:?}: {e}"));
        println!("wrote {} ({size}x{size})", path.display());
    }

    println!();
    println!("iconset: {}", iconset_dir.display());
    println!(
        "next: iconutil -c icns {} -o {}",
        iconset_dir.display(),
        repo_root.join("assets").join("AppIcon.icns").display()
    );
}

/// Render the full app icon (rounded square + gradient + waveform glyph +
/// shadow) as straight-alpha RGBA8 at `size` x `size`.
fn render_app_icon(size: u32) -> Vec<u8> {
    let n = size as usize;
    let sz = size as f32;
    let mut rgba = vec![0u8; n * n * 4];

    let inset = sz * SQUARE_INSET_FRAC;
    let square_side = sz - 2.0 * inset;
    let square_cx = sz / 2.0;
    let square_cy = sz / 2.0;
    let square_half = square_side / 2.0;
    let corner_r = square_side * CORNER_RADIUS_FRAC;

    let glyph_w = square_side * GLYPH_WIDTH_FRAC;
    let glyph_h = square_side * GLYPH_HEIGHT_FRAC;
    let bars = waveform_bars(square_cx, square_cy, glyph_w, glyph_h);
    let bar_w = bar_width(glyph_w);

    let shadow_dx = sz * SHADOW_OFFSET_FRAC;
    let shadow_dy = sz * SHADOW_OFFSET_FRAC * 1.5;

    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    let step = 1.0 / SUPERSAMPLE as f32;

    for y in 0..n {
        // Gradient is smooth (no hard edges), so a single pixel-center sample
        // is enough; only shape edges need supersampled AA.
        let gradient_rgb = gradient_at(y as f32 + 0.5, sz);

        for x in 0..n {
            let mut mask_hits = 0u32;
            let mut glyph_hits = 0u32;
            let mut shadow_hits = 0u32;

            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let px = x as f32 + (sx as f32 + 0.5) * step;
                    let py = y as f32 + (sy as f32 + 0.5) * step;

                    if inside_rounded_rect(
                        px,
                        py,
                        square_cx,
                        square_cy,
                        square_half,
                        square_half,
                        corner_r,
                    ) {
                        mask_hits += 1;
                    }
                    if bars
                        .iter()
                        .any(|&(bcx, bcy, bh)| inside_capsule(px, py, bcx, bcy, bar_w, bh))
                    {
                        glyph_hits += 1;
                    }
                    if bars.iter().any(|&(bcx, bcy, bh)| {
                        inside_capsule(px - shadow_dx, py - shadow_dy, bcx, bcy, bar_w, bh)
                    }) {
                        shadow_hits += 1;
                    }
                }
            }

            let mask_cov = mask_hits as f32 / samples;
            if mask_cov <= 0.0 {
                continue; // stays fully transparent
            }
            let glyph_cov = glyph_hits as f32 / samples;
            let shadow_cov = shadow_hits as f32 / samples;

            // Shadow tints the gradient (only where the glyph itself isn't
            // already fully opaque white), then the glyph is painted on top.
            let mut rgb = gradient_rgb;
            let shadow_amount = shadow_cov * SHADOW_STRENGTH * (1.0 - glyph_cov);
            rgb = lerp_rgb(rgb, [0x00, 0x00, 0x00], shadow_amount);
            rgb = lerp_rgb(rgb, [0xFF, 0xFF, 0xFF], glyph_cov);

            let alpha = (mask_cov * 255.0).round() as u8;
            let i = (y * n + x) * 4;
            rgba[i] = rgb[0];
            rgba[i + 1] = rgb[1];
            rgba[i + 2] = rgb[2];
            rgba[i + 3] = alpha;
        }
    }

    rgba
}

/// Vertical gradient color at canvas row `py` (top = `GRADIENT_TOP`, bottom =
/// `GRADIENT_BOTTOM`).
fn gradient_at(py: f32, sz: f32) -> [u8; 3] {
    let t = (py / sz).clamp(0.0, 1.0);
    lerp_rgb(GRADIENT_TOP, GRADIENT_BOTTOM, t)
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ]
}

/// The base (unweighted) bar width for a glyph of total width `glyph_w`,
/// matching [`waveform_bars`]'s layout math.
fn bar_width(glyph_w: f32) -> f32 {
    let bar_count = BAR_HEIGHT_FRACTIONS.len() as f32;
    glyph_w / (bar_count + (bar_count - 1.0) * GAP_RATIO)
}

/// The five `(center_x, center_y, height)` waveform bars, centered at
/// `(cx, cy)` within a `glyph_w` x `glyph_h` box.
fn waveform_bars(cx: f32, cy: f32, glyph_w: f32, glyph_h: f32) -> [(f32, f32, f32); 5] {
    let base_bar_w = bar_width(glyph_w);
    let pitch = base_bar_w * (1.0 + GAP_RATIO);
    let left_x = cx - glyph_w / 2.0;

    let mut bars = [(0.0f32, 0.0f32, 0.0f32); 5];
    for (i, frac) in BAR_HEIGHT_FRACTIONS.iter().enumerate() {
        let bcx = left_x + base_bar_w / 2.0 + (i as f32) * pitch;
        let h = glyph_h * frac;
        bars[i] = (bcx, cy, h);
    }
    bars
}

/// True if `(px, py)` lies within a rounded rectangle centered at `(cx, cy)`
/// with half-extents `(half_w, half_h)` and corner radius `r`.
fn inside_rounded_rect(
    px: f32,
    py: f32,
    cx: f32,
    cy: f32,
    half_w: f32,
    half_h: f32,
    r: f32,
) -> bool {
    let dx = ((px - cx).abs() - (half_w - r)).max(0.0);
    let dy = ((py - cy).abs() - (half_h - r)).max(0.0);
    (dx * dx + dy * dy).sqrt() <= r
}

/// True if `(px, py)` lies within a capsule (a `w` x `h` rectangle with
/// semicircular caps of radius `w / 2`) centered at `(cx, cy)`.
fn inside_capsule(px: f32, py: f32, cx: f32, cy: f32, w: f32, h: f32) -> bool {
    inside_rounded_rect(px, py, cx, cy, w / 2.0, h / 2.0, w / 2.0)
}

/// Write an RGBA8 buffer as a PNG to `path`.
fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), png::EncodingError> {
    let file = fs::File::create(path).map_err(png::EncodingError::IoError)?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)
}
