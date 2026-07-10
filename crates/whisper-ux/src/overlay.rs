//! Floating waveform overlay (D-0013, Wispr Flow-style). A small borderless,
//! transparent, always-on-top capsule that appears while a dictation session is
//! live and shows a voice-reactive waveform, a recording dot, and an elapsed
//! timer; during post-capture processing it freezes/dims with a "…" affordance
//! so the pipeline finishing is visible.
//!
//! The window lifecycle (a second egui viewport) lives in `gui.rs`; this module
//! owns the *pure* pieces so they are unit-tested without a display:
//! - [`overlay_rect`]: where the capsule goes on the monitor (centered + edge);
//! - [`LevelRing`] + [`bar_heights`]/[`fill_bar_heights`]: the 10 Hz RMS ring and
//!   its mapping to N eased bar heights with an idle "breathing" baseline;
//! - [`approach`]: the per-frame lerp that smooths 10 Hz steps to 60 fps;
//! - [`format_elapsed`]: the mm:ss timer.
//!
//! The paint code ([`Overlay::paint`]) stays thin and calls into these.

use std::collections::VecDeque;

use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2};

use crate::shell_settings::OverlayPosition;

/// Number of waveform bars in the capsule.
pub const BAR_COUNT: usize = 24;

/// Logical size of the overlay window (>= the 280x64 floor in the brief).
pub const OVERLAY_SIZE: Vec2 = Vec2::new(320.0, 72.0);

/// Fallback monitor size used when the platform reports no monitor dimensions
/// (e.g. headless, or before the first frame): a conservative laptop default.
pub const FALLBACK_MONITOR: Vec2 = Vec2::new(1440.0, 900.0);

/// Margin below the menu bar for [`OverlayPosition::Top`].
const TOP_MARGIN: f32 = 8.0;
/// Margin above the Dock for [`OverlayPosition::Bottom`].
const BOTTOM_MARGIN: f32 = 64.0;

// Waveform tuning.
/// Gain applied to raw RMS (typically ~0.05..0.3) so speech fills the capsule.
const SIGNAL_GAIN: f32 = 2.6;
/// Baseline height so silence isn't a dead flatline.
const IDLE_BASE: f32 = 0.06;
/// Amplitude of the idle breathing wave on top of the baseline.
const IDLE_AMP: f32 = 0.05;
/// Angular speed of the breathing wave (rad/s).
const BREATH_SPEED: f32 = 2.2;
/// Per-bar phase offset so the baseline travels as a gentle wave.
const BREATH_WAVE: f32 = 0.55;
/// Per-frame lerp factor from displayed toward target (higher = snappier).
const LERP_ALPHA: f32 = 0.35;

// Palette (kept local; the capsule is its own translucent surface, not a card).
const CAPSULE_FILL: Color32 = Color32::from_rgba_premultiplied(16, 18, 22, 232);
const CAPSULE_STROKE: Color32 = Color32::from_rgb(58, 62, 70);
const TEXT_COLOR: Color32 = Color32::from_rgb(220, 224, 230);
const TEXT_DIM: Color32 = Color32::from_rgb(140, 146, 154);
const REC_RED: Color32 = Color32::from_rgb(0xE0, 0x3B, 0x24);
/// Dimmed bar tone used while processing (waveform frozen).
const BAR_DIM: Color32 = Color32::from_rgb(88, 96, 100);

/// Left reserve for the recording dot + timer / processing "…".
const LEFT_CLUSTER_W: f32 = 64.0;
/// Right/edge padding for the bar field.
const EDGE_PAD: f32 = 12.0;

/// Visual mode of the overlay, derived from the tray state by `gui.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayPhase {
    /// Live capture: waveform animates, red dot + timer shown.
    Recording,
    /// Post-capture pipeline: waveform frozen/dimmed, "…" shown.
    Processing,
}

/// Compute the overlay window's top-left position on the monitor: horizontally
/// centered, hugging the configured vertical edge, clamped so it never leaves
/// the screen. Pure so it is unit-tested without a display.
#[must_use]
pub fn overlay_rect(monitor: Vec2, size: Vec2, pos: OverlayPosition) -> Pos2 {
    // Guard against a degenerate/None-fallback monitor smaller than the capsule.
    let max_x = (monitor.x - size.x).max(0.0);
    let max_y = (monitor.y - size.y).max(0.0);

    let x = (monitor.x - size.x) * 0.5;
    let y = match pos {
        OverlayPosition::Top => TOP_MARGIN,
        OverlayPosition::Bottom => monitor.y - size.y - BOTTOM_MARGIN,
    };

    Pos2::new(x.clamp(0.0, max_x), y.clamp(0.0, max_y))
}

/// Fixed-capacity ring of recent RMS samples (newest last), fed from the daemon's
/// 10 Hz `audio.level` events.
#[derive(Debug, Clone)]
pub struct LevelRing {
    buf: VecDeque<f32>,
    cap: usize,
}

impl LevelRing {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap.max(1)),
            cap: cap.max(1),
        }
    }

    /// Push one RMS sample (clamped to `0.0..=1.0`), evicting the oldest when full.
    pub fn push(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        if self.buf.len() == self.cap {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Sample `back` steps from the newest (`back == 0` is the newest), or `None`
    /// if the ring holds fewer samples than that.
    #[must_use]
    pub fn nth_newest(&self, back: usize) -> Option<f32> {
        let len = self.buf.len();
        (back < len).then(|| self.buf[len - 1 - back])
    }
}

/// Fill `out` with N bar heights in `0.0..=1.0`: newest sample on the right,
/// older samples scrolling left, with an idle breathing baseline layered under
/// the (gained) signal so silence still shimmers. `t` is a monotonic time in
/// seconds driving the baseline wave. Pure and allocation-free.
pub fn fill_bar_heights(ring: &LevelRing, out: &mut [f32], t: f32) {
    let n = out.len();
    for (i, slot) in out.iter_mut().enumerate() {
        // Bar 0 is leftmost/oldest; bar n-1 is rightmost/newest.
        let age = n - 1 - i;
        let signal = ring.nth_newest(age).unwrap_or(0.0);
        let signal = (signal * SIGNAL_GAIN).clamp(0.0, 1.0);
        let phase = t * BREATH_SPEED - i as f32 * BREATH_WAVE;
        let breath = IDLE_BASE + IDLE_AMP * (0.5 + 0.5 * phase.sin());
        *slot = signal.max(breath).clamp(0.0, 1.0);
    }
}

/// [`fill_bar_heights`] returning a fresh `Vec` (convenience for tests/callers
/// that don't keep a reusable buffer).
#[must_use]
pub fn bar_heights(ring: &LevelRing, n: usize, t: f32) -> Vec<f32> {
    let mut v = vec![0.0; n];
    fill_bar_heights(ring, &mut v, t);
    v
}

/// Move `current` a fraction `alpha` toward `target` (per-frame lerp).
#[must_use]
pub fn approach(current: f32, target: f32, alpha: f32) -> f32 {
    current + (target - current) * alpha.clamp(0.0, 1.0)
}

/// Format an elapsed duration as `mm:ss` (minutes uncapped for long sessions).
#[must_use]
pub fn format_elapsed(total_secs: u64) -> String {
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}

/// Live, per-frame overlay state (ring + eased bar buffers + last peak).
/// Held by `gui.rs`; visibility and the elapsed timer are supplied by the caller
/// since they depend on the tray state machine and wall-clock.
pub struct Overlay {
    ring: LevelRing,
    target: Vec<f32>,
    displayed: Vec<f32>,
    last_peak: f32,
}

impl Default for Overlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ring: LevelRing::new(BAR_COUNT),
            target: vec![0.0; BAR_COUNT],
            displayed: vec![0.0; BAR_COUNT],
            last_peak: 0.0,
        }
    }

    /// Feed one 10 Hz `audio.level` sample.
    pub fn push_level(&mut self, rms: f32, peak: f32) {
        self.ring.push(rms);
        self.last_peak = peak.clamp(0.0, 1.0);
    }

    /// Reset the waveform between sessions.
    pub fn reset(&mut self) {
        self.ring.clear();
        self.last_peak = 0.0;
        for v in &mut self.displayed {
            *v = 0.0;
        }
        for v in &mut self.target {
            *v = 0.0;
        }
    }

    /// Paint the capsule for the current frame. `t` is a monotonic time in
    /// seconds (for the breathing wave), `elapsed_secs` drives the timer.
    pub fn paint(&mut self, ui: &mut egui::Ui, phase: OverlayPhase, t: f32, elapsed_secs: u64) {
        let rect = ui.max_rect();
        let painter = ui.painter().clone();
        let radius = rect.height() * 0.5;

        // Capsule surface (its own translucent rounded rect; the viewport itself
        // is transparent so the corners read as fully clear).
        painter.rect(
            rect,
            radius,
            CAPSULE_FILL,
            Stroke::new(1.0, CAPSULE_STROKE),
            StrokeKind::Inside,
        );

        // Advance the waveform. While recording, targets track the live ring;
        // while processing, targets freeze so the last shape lingers, dimmed.
        if phase == OverlayPhase::Recording {
            fill_bar_heights(&self.ring, &mut self.target, t);
        }
        for (d, tg) in self.displayed.iter_mut().zip(self.target.iter()) {
            *d = approach(*d, *tg, LERP_ALPHA);
        }

        let cy = rect.center().y;
        let cluster_x = rect.left() + 14.0;
        match phase {
            OverlayPhase::Recording => {
                painter.circle_filled(Pos2::new(cluster_x, cy), 4.0, REC_RED);
                painter.text(
                    Pos2::new(cluster_x + 12.0, cy),
                    Align2::LEFT_CENTER,
                    format_elapsed(elapsed_secs),
                    FontId::monospace(13.0),
                    TEXT_COLOR,
                );
            }
            OverlayPhase::Processing => {
                painter.text(
                    Pos2::new(cluster_x, cy),
                    Align2::LEFT_CENTER,
                    "…",
                    FontId::proportional(20.0),
                    TEXT_DIM,
                );
            }
        }

        // Bar field to the right of the left cluster.
        let bars_left = rect.left() + LEFT_CLUSTER_W;
        let bars_right = rect.right() - EDGE_PAD;
        let n = self.displayed.len();
        let area_w = (bars_right - bars_left).max(1.0);
        let gap = 2.0;
        let bar_w = ((area_w - gap * (n as f32 - 1.0)) / n as f32).max(1.0);
        let max_h = (rect.height() - 18.0).max(2.0);
        let base_color = match phase {
            OverlayPhase::Recording => crate::theme::ACCENT,
            OverlayPhase::Processing => BAR_DIM,
        };

        for (i, &h) in self.displayed.iter().enumerate() {
            let x = bars_left + i as f32 * (bar_w + gap) + bar_w * 0.5;
            let bar_h = (h * max_h).max(2.0);
            let bar = Rect::from_center_size(Pos2::new(x, cy), Vec2::new(bar_w, bar_h));
            // Peak brightens the newest (rightmost) bar's tip while recording.
            let color = if phase == OverlayPhase::Recording && i == n - 1 {
                lighten(base_color, self.last_peak)
            } else {
                base_color
            };
            painter.rect_filled(bar, bar_w * 0.5, color);
        }
    }
}

/// Blend `c` toward white by fraction `f` (for the peak-driven tip glow).
fn lighten(c: Color32, f: f32) -> Color32 {
    let f = f.clamp(0.0, 1.0);
    let mix = |a: u8| (a as f32 + (255.0 - a as f32) * f).round() as u8;
    Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_is_horizontally_centered() {
        let m = Vec2::new(1000.0, 800.0);
        let s = Vec2::new(300.0, 70.0);
        let top = overlay_rect(m, s, OverlayPosition::Top);
        let bottom = overlay_rect(m, s, OverlayPosition::Bottom);
        assert!((top.x - 350.0).abs() < 1e-3);
        assert!((bottom.x - 350.0).abs() < 1e-3);
    }

    #[test]
    fn top_and_bottom_pick_correct_edges() {
        let m = Vec2::new(1000.0, 800.0);
        let s = Vec2::new(300.0, 70.0);
        assert!((overlay_rect(m, s, OverlayPosition::Top).y - TOP_MARGIN).abs() < 1e-3);
        // 800 - 70 - 64 = 666
        assert!((overlay_rect(m, s, OverlayPosition::Bottom).y - 666.0).abs() < 1e-3);
    }

    #[test]
    fn tiny_monitor_clamps_on_screen() {
        // Monitor smaller than the capsule must never push it negative/off-screen.
        let m = Vec2::new(100.0, 40.0);
        let s = OVERLAY_SIZE;
        for pos in [OverlayPosition::Top, OverlayPosition::Bottom] {
            let p = overlay_rect(m, s, pos);
            assert!(p.x >= 0.0 && p.y >= 0.0, "pos {pos:?} -> {p:?}");
        }
    }

    #[test]
    fn fallback_monitor_centers_without_panicking() {
        let p = overlay_rect(FALLBACK_MONITOR, OVERLAY_SIZE, OverlayPosition::Bottom);
        let expect_x = (FALLBACK_MONITOR.x - OVERLAY_SIZE.x) * 0.5;
        assert!((p.x - expect_x).abs() < 1e-3);
    }

    #[test]
    fn ring_keeps_newest_and_evicts_oldest() {
        let mut r = LevelRing::new(3);
        assert!(r.is_empty());
        for v in [0.1, 0.2, 0.3, 0.4] {
            r.push(v);
        }
        assert_eq!(r.len(), 3);
        assert_eq!(r.nth_newest(0), Some(0.4));
        assert_eq!(r.nth_newest(2), Some(0.2));
        assert_eq!(r.nth_newest(3), None);
    }

    #[test]
    fn ring_clamps_samples() {
        let mut r = LevelRing::new(2);
        r.push(-1.0);
        r.push(5.0);
        assert_eq!(r.nth_newest(1), Some(0.0));
        assert_eq!(r.nth_newest(0), Some(1.0));
    }

    #[test]
    fn bars_put_newest_on_the_right() {
        let mut r = LevelRing::new(BAR_COUNT);
        // Loud newest sample, silence before it.
        r.push(1.0);
        let bars = bar_heights(&r, BAR_COUNT, 0.0);
        assert_eq!(bars.len(), BAR_COUNT);
        // Rightmost bar (newest) is the loudest.
        let rightmost = bars[BAR_COUNT - 1];
        assert!(rightmost >= 0.9, "rightmost {rightmost}");
        for &b in &bars {
            assert!((0.0..=1.0).contains(&b), "bar out of range: {b}");
        }
    }

    #[test]
    fn empty_ring_still_breathes_above_zero() {
        let r = LevelRing::new(BAR_COUNT);
        let bars = bar_heights(&r, BAR_COUNT, 0.0);
        assert!(
            bars.iter().all(|&b| b >= IDLE_BASE - 1e-6),
            "silence should not be a flat zero line"
        );
    }

    #[test]
    fn breathing_moves_over_time() {
        let r = LevelRing::new(BAR_COUNT);
        let a = bar_heights(&r, BAR_COUNT, 0.0);
        let b = bar_heights(&r, BAR_COUNT, 0.5);
        assert!(a != b, "baseline should animate with t");
    }

    #[test]
    fn approach_moves_fractionally_and_clamps_alpha() {
        assert!((approach(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
        // alpha > 1 is clamped to a full step, never overshoots.
        assert!((approach(0.0, 1.0, 2.0) - 1.0).abs() < 1e-6);
        assert!((approach(0.4, 0.4, 0.3) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn elapsed_formats_mm_ss() {
        assert_eq!(format_elapsed(0), "00:00");
        assert_eq!(format_elapsed(9), "00:09");
        assert_eq!(format_elapsed(75), "01:15");
        assert_eq!(format_elapsed(3661), "61:01");
    }

    #[test]
    fn lighten_moves_toward_white() {
        let c = Color32::from_rgb(0, 100, 200);
        assert_eq!(lighten(c, 0.0), c);
        assert_eq!(lighten(c, 1.0), Color32::from_rgb(255, 255, 255));
    }

    #[test]
    fn reset_clears_ring_and_bars() {
        let mut o = Overlay::new();
        o.push_level(0.8, 0.9);
        o.target[0] = 0.5;
        o.displayed[0] = 0.5;
        o.reset();
        assert!(o.ring.is_empty());
        assert!(o.displayed.iter().all(|&v| v == 0.0));
        assert!(o.target.iter().all(|&v| v == 0.0));
    }
}
