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

    use super::{downmix_to_mono, CaptureError, StreamResampler};

    /// Called (once) from the CoreAudio error callback when the device drops mid-session. The
    /// daemon wires this to a `audio.device_lost` WS event; this crate stays iface-free (D-0012).
    pub type DeviceLostCb = Box<dyn Fn(String) + Send + 'static>;

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
            on_device_lost: DeviceLostCb,
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

            let stream = match sample_format {
                cpal::SampleFormat::F32 => {
                    let mut resampler = resampler;
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
                                if !res.is_empty() {
                                    let _ = tx.send(res);
                                }
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::I16 => {
                    let mut resampler = resampler;
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
                                if !res.is_empty() {
                                    let _ = tx.send(res);
                                }
                            }
                        },
                        err_fn,
                        None,
                    )
                }
                cpal::SampleFormat::U16 => {
                    let mut resampler = resampler;
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
                                if !res.is_empty() {
                                    let _ = tx.send(res);
                                }
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

    #[test]
    fn capture_error_classifies_permission() {
        assert!(CaptureError::PermissionDenied("x".into()).is_permission_denied());
        assert!(!CaptureError::Backend("x".into()).is_permission_denied());
        let audio: AudioError = CaptureError::NoDevice("none".into()).into();
        assert!(matches!(audio, AudioError::StartFailed(_)));
    }
}
