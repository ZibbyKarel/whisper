//! Mic capture: default CoreAudio input device → the [`AudioSource`](whisper_core::ports::AudioSource)
//! port, resampled to 16 kHz mono f32 (`ARCHITECTURE.md` §3.2/§5, D-0012).
//!
//! The DSP (channel downmix, sample-rate conversion via `rubato`) is pure and unit-tested on
//! synthetic data; the CoreAudio device + stream live behind `#[cfg(target_os = "macos")]` and
//! are only reachable through hardware (`#[ignore]`) tests — opening an input device triggers
//! the macOS microphone TCC prompt, which CI cannot grant (D-0012).

use whisper_core::error::AudioError;

/// The pipeline's fixed working rate (`ARCHITECTURE.md` §3.2): whisper + Silero VAD run at
/// 16 kHz mono.
pub const TARGET_SAMPLE_RATE_HZ: u32 = 16_000;

/// Errors opening or configuring the capture device. Distinguishes microphone-permission denial
/// (surfaced upward as a `platform.permission` event, `ARCHITECTURE.md` §5 — never a blocking
/// dialog in core) from device/config/backend failures.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("microphone permission denied: {0}")]
    PermissionDenied(String),
    #[error("no default input device: {0}")]
    NoDevice(String),
    #[error("audio config error: {0}")]
    Config(String),
    #[error("audio backend error: {0}")]
    Backend(String),
}

impl CaptureError {
    /// Whether this is a microphone-permission denial (drives the `platform.permission` event).
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, CaptureError::PermissionDenied(_))
    }
}

impl From<CaptureError> for AudioError {
    fn from(e: CaptureError) -> Self {
        // The pipeline only knows `DeviceLost`/`StartFailed`; a failure to open the device is a
        // start failure. Permission classification is preserved at the daemon boundary before
        // this conversion (it emits `platform.permission`).
        AudioError::StartFailed(e.to_string())
    }
}

/// Downmix interleaved multi-channel f32 samples to mono by averaging each frame's channels.
/// Mono input is returned unchanged.
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let ch = channels as usize;
    interleaved
        .chunks(ch)
        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
        .collect()
}

/// Streaming resampler to [`TARGET_SAMPLE_RATE_HZ`] mono (`rubato`, D-0012). Accepts
/// arbitrary-length mono chunks (as a CoreAudio callback delivers them), buffering across calls;
/// a device already at 16 kHz is a zero-cost pass-through.
pub struct StreamResampler {
    // `None` ⇒ input already at the target rate (pass-through).
    resampler: Option<rubato::FastFixedIn<f32>>,
    in_buf: Vec<f32>,
    chunk: usize,
}

impl StreamResampler {
    /// Chunk size fed to rubato per process call. 1024 input frames balances latency and
    /// per-call overhead for typical CoreAudio buffer sizes.
    const CHUNK: usize = 1024;

    /// Build a resampler from `in_rate` Hz mono to [`TARGET_SAMPLE_RATE_HZ`].
    pub fn new(in_rate: u32) -> Result<Self, CaptureError> {
        if in_rate == TARGET_SAMPLE_RATE_HZ {
            return Ok(Self {
                resampler: None,
                in_buf: Vec::new(),
                chunk: 0,
            });
        }
        let ratio = TARGET_SAMPLE_RATE_HZ as f64 / in_rate as f64;
        let resampler = rubato::FastFixedIn::<f32>::new(
            ratio,
            1.0, // fixed ratio (no live retuning)
            rubato::PolynomialDegree::Cubic,
            Self::CHUNK,
            1, // mono
        )
        .map_err(|e| CaptureError::Config(format!("build resampler: {e}")))?;
        Ok(Self {
            resampler: Some(resampler),
            in_buf: Vec::new(),
            chunk: Self::CHUNK,
        })
    }

    /// Feed a mono chunk; returns any 16 kHz output produced (possibly empty until a full
    /// rubato chunk has accumulated).
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<f32>, CaptureError> {
        use rubato::Resampler;
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(samples.to_vec());
        };
        self.in_buf.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.in_buf.len() >= self.chunk {
            let chunk: Vec<f32> = self.in_buf.drain(..self.chunk).collect();
            let processed = resampler
                .process(&[chunk], None)
                .map_err(|e| CaptureError::Config(format!("resample: {e}")))?;
            out.extend_from_slice(&processed[0]);
        }
        Ok(out)
    }

    /// Flush the final partial chunk at end-of-capture (zero-padded for rubato; trailing silence
    /// is harmless — the VAD trims it, D-0008).
    pub fn finish(&mut self) -> Result<Vec<f32>, CaptureError> {
        use rubato::Resampler;
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(Vec::new());
        };
        if self.in_buf.is_empty() {
            return Ok(Vec::new());
        }
        let mut last: Vec<f32> = std::mem::take(&mut self.in_buf);
        last.resize(self.chunk, 0.0);
        let processed = resampler
            .process(&[last], None)
            .map_err(|e| CaptureError::Config(format!("resample flush: {e}")))?;
        Ok(processed[0].clone())
    }
}

/// AGC-lite tuning (`ARCHITECTURE.md` §3.2/§5): a boost-only automatic gain so a quiet mic is
/// lifted toward a usable level before the VAD/STT ever see it. Amplify-only (`gain >= 1.0`) so
/// loud input is never attenuated — the measured failure was *quiet* speech being lost, not loud
/// speech clipping. Dependency-free and deterministic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoGainConfig {
    /// Off ⇒ [`AutoGain::process`] is a pass-through. On by default.
    pub enabled: bool,
    /// Peak the gain chases the signal toward, in `[0.0, 1.0]`. ~0.7 leaves headroom below clip.
    pub target_peak: f32,
    /// Upper bound on the gain multiplier; the lower bound is fixed at 1.0 (boost-only).
    pub max_gain: f32,
}

impl Default for AutoGainConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_peak: 0.7,
            max_gain: 12.0,
        }
    }
}

/// Boost-only automatic gain control (AGC-lite). Runs on the 16 kHz mono capture stream after
/// downmix/resample, before chunks reach the pipeline. Per block it estimates the peak, chases a
/// target level with a fast attack (gain *down*, clip protection) / slow release (gain *up*), and
/// hard-limits the output so a transient during the attack ramp cannot clip. An RMS activity gate
/// stops the gain ratcheting up on silence/noise.
#[derive(Debug, Clone)]
pub struct AutoGain {
    cfg: AutoGainConfig,
    gain: f32,
}

impl AutoGain {
    /// Fixed lower bound: amplify-only, never attenuate below unity.
    const MIN_GAIN: f32 = 1.0;
    /// RMS below this (~ -46 dBFS) is treated as silence/room-noise: the gain is *held*, never
    /// raised, so a quiet room does not drive the gain to `max_gain`.
    const NOISE_FLOOR: f32 = 0.005;
    /// Guards the `target/peak` division against a near-zero peak inside an "active" block.
    const EPS: f32 = 1e-6;
    /// Attack time constant (samples ≈ 100 ms @ 16 kHz): how fast the gain *drops* when the signal
    /// gets louder — fast, so a sudden onset does not clip.
    const ATTACK_TAU: f32 = 1_600.0;
    /// Release time constant (samples ≈ 750 ms @ 16 kHz): how slowly the gain *rises* for quiet
    /// speech — slow, to avoid pumping and to keep noise from being amplified between words.
    const RELEASE_TAU: f32 = 12_000.0;
    /// Hard-limiter ceiling: post-gain samples are clamped to `±CEILING` so nothing clips.
    const CEILING: f32 = 0.99;

    pub fn new(cfg: AutoGainConfig) -> Self {
        Self { cfg, gain: 1.0 }
    }

    /// Current smoothed gain multiplier (for tests/telemetry).
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Apply gain to one block in place. `buf` is 16 kHz mono f32; length is arbitrary (a capture
    /// callback delivers variable-size chunks), and the smoothing is length-aware so behaviour does
    /// not depend on the block size.
    pub fn process(&mut self, buf: &mut [f32]) {
        if !self.cfg.enabled || buf.is_empty() {
            return;
        }
        let n = buf.len();
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        for &s in buf.iter() {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
            sum_sq += (s as f64) * (s as f64);
        }
        let rms = (sum_sq / n as f64).sqrt() as f32;

        let max_gain = self.cfg.max_gain.max(Self::MIN_GAIN);
        // Only chase a new target when the block carries speech-level energy (activity gate);
        // otherwise hold the current gain so silence/noise never ratchets it up.
        let target_gain = if rms > Self::NOISE_FLOOR && peak > Self::EPS {
            (self.cfg.target_peak / peak).clamp(Self::MIN_GAIN, max_gain)
        } else {
            self.gain
        };

        // Fast attack when reducing gain (loud onset → clip risk), slow release when raising it.
        let tau = if target_gain < self.gain {
            Self::ATTACK_TAU
        } else {
            Self::RELEASE_TAU
        };
        let alpha = 1.0 - (-(n as f32) / tau).exp();
        let new_gain = self.gain + alpha * (target_gain - self.gain);

        // Per-sample linear ramp old→new avoids a zipper artefact at the block boundary.
        let step = (new_gain - self.gain) / n as f32;
        let mut g = self.gain;
        for s in buf.iter_mut() {
            g += step;
            *s = (*s * g).clamp(-Self::CEILING, Self::CEILING);
        }
        self.gain = new_gain;
    }
}

/// A post-gain level sample (`audio.level` telemetry). Both fields are linear amplitude in
/// `[0.0, 1.0]`; the daemon turns this into the `audio.level` WS event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioLevel {
    pub rms: f32,
    pub peak: f32,
}

/// Windowed RMS/peak meter feeding the throttled `audio.level` event. Accumulates samples and
/// emits one [`AudioLevel`] every `window` samples (default ~10 Hz), so the capture path reports
/// input level without flooding the WS channel.
#[derive(Debug, Clone)]
pub struct LevelMeter {
    window: usize,
    count: usize,
    sum_sq: f64,
    peak: f32,
}

impl LevelMeter {
    pub fn new(window: usize) -> Self {
        Self {
            window: window.max(1),
            count: 0,
            sum_sq: 0.0,
            peak: 0.0,
        }
    }

    /// ~10 Hz at the pipeline's 16 kHz working rate (one sample per 100 ms).
    pub fn at_10hz() -> Self {
        Self::new(TARGET_SAMPLE_RATE_HZ as usize / 10)
    }

    /// Feed a block; returns one [`AudioLevel`] per completed window (usually 0 or 1, but a block
    /// larger than the window can complete several).
    pub fn push(&mut self, samples: &[f32]) -> Vec<AudioLevel> {
        let mut out = Vec::new();
        for &s in samples {
            let a = s.abs();
            if a > self.peak {
                self.peak = a;
            }
            self.sum_sq += (s as f64) * (s as f64);
            self.count += 1;
            if self.count >= self.window {
                out.push(AudioLevel {
                    rms: (self.sum_sq / self.count as f64).sqrt() as f32,
                    peak: self.peak,
                });
                self.count = 0;
                self.sum_sq = 0.0;
                self.peak = 0.0;
            }
        }
        out
    }
}

#[cfg(target_os = "macos")]
pub use mac::CpalAudioSource;

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, TryRecvError};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use whisper_core::error::AudioError;
    use whisper_core::ports::{AudioSource, AudioStream};
    use whisper_core::types::{AudioCfg, AudioFrame};

    use super::{
        downmix_to_mono, AudioLevel, AutoGain, AutoGainConfig, CaptureError, LevelMeter,
        StreamResampler,
    };

    /// Called (once) from the CoreAudio error callback when the device drops mid-session. The
    /// daemon wires this to a `audio.device_lost` WS event; this crate stays iface-free (D-0012).
    pub type DeviceLostCb = Box<dyn Fn(String) + Send + 'static>;

    /// Called (throttled, ~10 Hz) from the capture callback with the post-gain input level. The
    /// daemon maps it to an `audio.level` WS event; this crate stays iface-free (D-0012).
    pub type LevelCb = Box<dyn Fn(AudioLevel) + Send + 'static>;

    /// Live microphone [`AudioSource`]: opens the default CoreAudio input device eagerly and
    /// streams 16 kHz mono f32 frames pulled by the pipeline. Eager open means a permission /
    /// device error surfaces at construction (where the daemon can classify it), not deep inside
    /// the pipeline.
    pub struct CpalAudioSource {
        // Taken by `start`; the whole capture (incl. the `!Send` cpal `Stream`) stays on the one
        // worker thread that built and drives it (D-0011/D-0012).
        inner: Mutex<Option<Capture>>,
    }

    struct Capture {
        _stream: cpal::Stream,
        rx: Receiver<Vec<f32>>,
        error: Arc<Mutex<Option<String>>>,
        stop: Arc<AtomicBool>,
    }

    impl CpalAudioSource {
        /// Open the default input device and start capturing. `stop` ends capture gracefully
        /// (the pull side drains buffered audio, then reports end-of-stream); `on_device_lost`
        /// fires if the device drops mid-session.
        pub fn open(
            stop: Arc<AtomicBool>,
            agc: AutoGainConfig,
            on_device_lost: DeviceLostCb,
            on_level: LevelCb,
        ) -> Result<Self, CaptureError> {
            let host = cpal::default_host();
            let device = host
                .default_input_device()
                .ok_or_else(|| CaptureError::NoDevice("host has no default input device".into()))?;
            let default_cfg = device
                .default_input_config()
                .map_err(|e| classify(&e.to_string()))?;
            let sample_format = default_cfg.sample_format();
            let config: cpal::StreamConfig = default_cfg.into();
            let in_rate = config.sample_rate.0;
            let channels = config.channels;

            let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
            let error = Arc::new(Mutex::new(None));

            // Single error callback: records the reason (so `next_frame` reports `DeviceLost`)
            // and notifies the daemon. Moved into whichever stream-format arm runs (arms are
            // mutually exclusive, so moving the same closure into each is sound).
            let error_for_cb = error.clone();
            let err_fn = move |err: cpal::StreamError| {
                let reason = err.to_string();
                *error_for_cb.lock().unwrap() = Some(reason.clone());
                (on_device_lost)(reason);
            };

            let resampler = StreamResampler::new(in_rate)?;
            let agc = AutoGain::new(agc);
            let meter = LevelMeter::at_10hz();

            let stream = match sample_format {
                cpal::SampleFormat::F32 => {
                    let mut resampler = resampler;
                    let mut agc = agc;
                    let mut meter = meter;
                    let stop = stop.clone();
                    let tx = tx.clone();
                    device.build_input_stream(
                        &config,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            if stop.load(Ordering::SeqCst) {
                                return;
                            }
                            let mono = downmix_to_mono(data, channels);
                            if let Ok(res) = resampler.push(&mono) {
                                emit(&mut agc, &mut meter, &on_level, &tx, res);
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::I16 => {
                    let mut resampler = resampler;
                    let mut agc = agc;
                    let mut meter = meter;
                    let stop = stop.clone();
                    let tx = tx.clone();
                    device.build_input_stream(
                        &config,
                        move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            if stop.load(Ordering::SeqCst) {
                                return;
                            }
                            let floats: Vec<f32> =
                                data.iter().map(|&s| s as f32 / 32768.0).collect();
                            let mono = downmix_to_mono(&floats, channels);
                            if let Ok(res) = resampler.push(&mono) {
                                emit(&mut agc, &mut meter, &on_level, &tx, res);
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::U16 => {
                    let mut resampler = resampler;
                    let mut agc = agc;
                    let mut meter = meter;
                    let stop = stop.clone();
                    let tx = tx.clone();
                    device.build_input_stream(
                        &config,
                        move |data: &[u16], _: &cpal::InputCallbackInfo| {
                            if stop.load(Ordering::SeqCst) {
                                return;
                            }
                            let floats: Vec<f32> = data
                                .iter()
                                .map(|&s| (s as f32 - 32768.0) / 32768.0)
                                .collect();
                            let mono = downmix_to_mono(&floats, channels);
                            if let Ok(res) = resampler.push(&mono) {
                                emit(&mut agc, &mut meter, &on_level, &tx, res);
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                other => {
                    return Err(CaptureError::Config(format!(
                        "unsupported input sample format {other:?}"
                    )))
                }
            }
            .map_err(|e| classify_build(&e))?;

            stream
                .play()
                .map_err(|e| CaptureError::Backend(format!("stream play: {e}")))?;

            Ok(Self {
                inner: Mutex::new(Some(Capture {
                    _stream: stream,
                    rx,
                    error,
                    stop,
                })),
            })
        }
    }

    impl AudioSource for CpalAudioSource {
        fn start(&self, _cfg: AudioCfg) -> Result<Box<dyn AudioStream>, AudioError> {
            let capture = self
                .inner
                .lock()
                .expect("cpal capture mutex")
                .take()
                .ok_or_else(|| AudioError::StartFailed("cpal source already consumed".into()))?;
            Ok(Box::new(CpalAudioStream { capture }))
        }
    }

    struct CpalAudioStream {
        capture: Capture,
    }

    impl AudioStream for CpalAudioStream {
        fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioError> {
            loop {
                if let Some(reason) = self.capture.error.lock().expect("error mutex").take() {
                    return Err(AudioError::DeviceLost { reason });
                }
                match self.capture.rx.try_recv() {
                    Ok(chunk) => return Ok(Some(chunk)),
                    Err(TryRecvError::Empty) => {
                        if self.capture.stop.load(Ordering::SeqCst) {
                            // Graceful stop: give the last callback a moment to flush, drain one
                            // more chunk if present, then report end-of-stream so the pipeline
                            // proceeds to VAD flush → STT → delivery (D-0012 graceful stop).
                            std::thread::sleep(Duration::from_millis(20));
                            return Ok(self.capture.rx.try_recv().ok());
                        }
                        // No live paste-completion / frame-ready signal is exposed by cpal; poll.
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(TryRecvError::Disconnected) => return Ok(None),
                }
            }
        }
    }

    /// Post-process one resampled 16 kHz block: apply AGC in place, feed the throttled level
    /// meter (firing `on_level` per completed window), then hand the boosted chunk to the pull
    /// side. Empty blocks (the resampler is still accumulating) are dropped.
    fn emit(
        agc: &mut AutoGain,
        meter: &mut LevelMeter,
        on_level: &LevelCb,
        tx: &std::sync::mpsc::Sender<Vec<f32>>,
        mut res: Vec<f32>,
    ) {
        if res.is_empty() {
            return;
        }
        agc.process(&mut res);
        for lvl in meter.push(&res) {
            (on_level)(lvl);
        }
        let _ = tx.send(res);
    }

    /// Classify a `default_input_config` error string as permission vs. other.
    fn classify(msg: &str) -> CaptureError {
        if looks_like_permission(msg) {
            CaptureError::PermissionDenied(msg.to_string())
        } else {
            CaptureError::Backend(msg.to_string())
        }
    }

    /// Classify a `build_input_stream` error, preserving permission denial.
    fn classify_build(err: &cpal::BuildStreamError) -> CaptureError {
        let msg = err.to_string();
        if looks_like_permission(&msg) {
            CaptureError::PermissionDenied(msg)
        } else {
            CaptureError::Backend(msg)
        }
    }

    /// Heuristic: macOS surfaces microphone denial through a few different OSStatus / message
    /// shapes. Best-effort — the manual checklist verifies the real denial path (D-0012).
    fn looks_like_permission(msg: &str) -> bool {
        let m = msg.to_ascii_lowercase();
        m.contains("permission")
            || m.contains("not authorized")
            || m.contains("access")
            || m.contains("denied")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_stereo_to_mono() {
        // Interleaved L,R,L,R → per-frame average.
        let stereo = [1.0, 0.0, 0.5, 0.5, -1.0, 1.0];
        assert_eq!(downmix_to_mono(&stereo, 2), vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn downmix_passes_mono_through_unchanged() {
        let mono = [0.1, -0.2, 0.3];
        assert_eq!(downmix_to_mono(&mono, 1), mono.to_vec());
    }

    #[test]
    fn resampler_is_passthrough_at_target_rate() {
        let mut r = StreamResampler::new(TARGET_SAMPLE_RATE_HZ).unwrap();
        let input: Vec<f32> = (0..100).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = r.push(&input).unwrap();
        assert_eq!(out, input);
        assert!(r.finish().unwrap().is_empty());
    }

    #[test]
    fn resampler_downsamples_48k_to_16k_by_ratio() {
        // Feed 1 s of 48 kHz sine; expect ~16 k output samples (ratio 1/3) within a small
        // boundary tolerance, and no panic / no NaNs.
        let mut r = StreamResampler::new(48_000).unwrap();
        let n_in = 48_000usize;
        let input: Vec<f32> = (0..n_in)
            .map(|i| (i as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin())
            .collect();
        let mut out = r.push(&input).unwrap();
        out.extend(r.finish().unwrap());
        let expected = n_in / 3;
        let diff = (out.len() as isize - expected as isize).unsigned_abs();
        assert!(
            diff <= StreamResampler::CHUNK,
            "expected ~{expected} samples, got {} (diff {diff})",
            out.len()
        );
        assert!(out.iter().all(|s| s.is_finite()), "resampled NaN/inf");
    }

    #[test]
    fn resampler_upsamples_8k_to_16k_by_ratio() {
        let mut r = StreamResampler::new(8_000).unwrap();
        let n_in = 8_000usize;
        let input: Vec<f32> = (0..n_in).map(|i| (i as f32 * 0.001).sin()).collect();
        let mut out = r.push(&input).unwrap();
        out.extend(r.finish().unwrap());
        let expected = n_in * 2;
        let diff = (out.len() as isize - expected as isize).unsigned_abs();
        assert!(diff <= 2 * StreamResampler::CHUNK, "got {}", out.len());
    }

    // ---- AGC-lite (AutoGain) ----------------------------------------------------------------

    fn sine_block(peak: f32, n: usize, phase: &mut f32) -> Vec<f32> {
        let step = 440.0 / TARGET_SAMPLE_RATE_HZ as f32 * std::f32::consts::TAU;
        (0..n)
            .map(|_| {
                let s = peak * phase.sin();
                *phase += step;
                s
            })
            .collect()
    }

    fn block_peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0, |m, &s| m.max(s.abs()))
    }

    #[test]
    fn agc_amplifies_quiet_signal_toward_target() {
        let cfg = AutoGainConfig::default();
        let mut agc = AutoGain::new(cfg);
        let mut phase = 0.0f32;
        // Quiet input (peak 0.1) whose target gain (0.7/0.1 = 7×) is within [1, 12].
        let mut last = Vec::new();
        for _ in 0..80 {
            let mut b = sine_block(0.1, 1_600, &mut phase);
            agc.process(&mut b);
            last = b;
        }
        let peak = block_peak(&last);
        assert!(
            (0.6..=0.78).contains(&peak),
            "quiet signal not lifted toward target: peak {peak}, gain {}",
            agc.gain()
        );
    }

    #[test]
    fn agc_leaves_loud_signal_unchanged() {
        let cfg = AutoGainConfig::default();
        let mut agc = AutoGain::new(cfg);
        let mut phase = 0.0f32;
        // Loud input (peak 0.9): desired gain < 1, so the boost-only AGC holds unity.
        for _ in 0..40 {
            let mut b = sine_block(0.9, 1_600, &mut phase);
            agc.process(&mut b);
            assert!(block_peak(&b) <= 0.99, "loud signal clipped past ceiling");
        }
        assert!(
            (agc.gain() - 1.0).abs() < 1e-3,
            "loud signal was amplified: gain {}",
            agc.gain()
        );
    }

    #[test]
    fn agc_gate_holds_gain_on_silence() {
        let cfg = AutoGainConfig::default();
        let mut agc = AutoGain::new(cfg);
        // Pure digital silence must never ratchet the gain up.
        for _ in 0..200 {
            let mut b = vec![0.0f32; 1_600];
            agc.process(&mut b);
        }
        assert!(
            (agc.gain() - 1.0).abs() < 1e-6,
            "silence raised the gain to {}",
            agc.gain()
        );
        // Sub-floor noise is also gated (no runaway gain).
        let mut phase = 0.0f32;
        for _ in 0..200 {
            let mut b = sine_block(0.002, 1_600, &mut phase);
            agc.process(&mut b);
        }
        assert!(
            agc.gain() < 1.001,
            "sub-floor noise ratcheted the gain to {}",
            agc.gain()
        );
    }

    #[test]
    fn agc_hard_limiter_never_clips() {
        let cfg = AutoGainConfig::default();
        let mut agc = AutoGain::new(cfg);
        let mut phase = 0.0f32;
        // Ramp gain up on quiet speech, then hit it with a loud onset: the attack ramp could
        // momentarily over-apply gain, but the hard limiter must keep every sample in range.
        for _ in 0..80 {
            let mut b = sine_block(0.08, 1_600, &mut phase);
            agc.process(&mut b);
        }
        let mut loud = sine_block(0.9, 1_600, &mut phase);
        agc.process(&mut loud);
        assert!(
            loud.iter().all(|&s| s.abs() <= 0.99 + 1e-6),
            "limiter allowed clipping"
        );
    }

    #[test]
    fn agc_disabled_is_passthrough() {
        let cfg = AutoGainConfig {
            enabled: false,
            ..AutoGainConfig::default()
        };
        let mut agc = AutoGain::new(cfg);
        let mut phase = 0.0f32;
        let original = sine_block(0.05, 512, &mut phase);
        let mut b = original.clone();
        agc.process(&mut b);
        assert_eq!(b, original, "disabled AGC altered the signal");
    }

    #[test]
    fn agc_respects_max_gain_ceiling() {
        let cfg = AutoGainConfig {
            max_gain: 4.0,
            ..AutoGainConfig::default()
        };
        let mut agc = AutoGain::new(cfg);
        let mut phase = 0.0f32;
        // Target would want 0.7/0.05 = 14×, but max_gain caps it at 4×.
        for _ in 0..300 {
            let mut b = sine_block(0.05, 1_600, &mut phase);
            agc.process(&mut b);
        }
        assert!(
            (agc.gain() - 4.0).abs() < 0.05,
            "gain exceeded max_gain: {}",
            agc.gain()
        );
    }

    // ---- LevelMeter -------------------------------------------------------------------------

    #[test]
    fn level_meter_emits_once_per_window() {
        let mut meter = LevelMeter::new(1_600);
        assert!(meter.push(&vec![0.0; 800]).is_empty(), "partial: no emit");
        let out = meter.push(&vec![0.0; 800]);
        assert_eq!(out.len(), 1, "window completed exactly once");
        // A block spanning several windows emits one level each.
        let out = meter.push(&vec![0.0; 3_200]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn level_meter_computes_rms_and_peak() {
        let mut meter = LevelMeter::new(4);
        // Constant 0.5 → rms 0.5, peak 0.5.
        let out = meter.push(&[0.5, -0.5, 0.5, -0.5]);
        assert_eq!(out.len(), 1);
        assert!((out[0].rms - 0.5).abs() < 1e-6);
        assert!((out[0].peak - 0.5).abs() < 1e-6);
    }

    #[test]
    fn capture_error_classifies_permission() {
        assert!(CaptureError::PermissionDenied("x".into()).is_permission_denied());
        assert!(!CaptureError::Backend("x".into()).is_permission_denied());
        let audio: AudioError = CaptureError::NoDevice("none".into()).into();
        assert!(matches!(audio, AudioError::StartFailed(_)));
    }
}
